use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration as ChronoDuration, Local, NaiveDate, NaiveTime, TimeZone, Utc};

use crate::{
    config::Schedule as ConfigSchedule,
    error::{Error, Result},
};

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // wired in by Phase 5 (the run loop)
pub struct Deadline(pub DateTime<Utc>);

#[allow(dead_code)] // wired in by Phase 5
impl Deadline {
    pub fn at(&self) -> DateTime<Utc> {
        self.0
    }
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.0
    }
    pub fn remaining(&self, now: DateTime<Utc>) -> StdDuration {
        let d = self.0 - now;
        if d <= ChronoDuration::zero() {
            StdDuration::ZERO
        } else {
            d.to_std().unwrap_or(StdDuration::ZERO)
        }
    }
}

/// Compute the deadline for a config schedule. `Config::validate` already
/// enforces that exactly one of `total_budget` / `deadline` is set.
#[allow(dead_code)] // wired in by Phase 5
pub fn compute_deadline(
    sched: &ConfigSchedule,
    now_utc: DateTime<Utc>,
    now_local: DateTime<Local>,
) -> Result<Deadline> {
    if let Some(d) = sched.total_budget {
        let cd = ChronoDuration::from_std(d)
            .map_err(|e| Error::Schedule(format!("invalid total_budget: {e}")))?;
        return Ok(Deadline(now_utc + cd));
    }
    let s = sched
        .deadline
        .as_deref()
        .ok_or_else(|| Error::Schedule("no deadline configured".into()))?;
    parse_deadline_expr(s, now_local).map(Deadline)
}

/// Parse a deadline string into an absolute UTC instant.
///
/// Supported forms:
///   - humantime duration:  "4h", "30m", "1d"           -> now + duration
///   - RFC3339:             "2026-05-21T09:00:00-07:00"
///   - natural language:    "tomorrow", "today 3pm", "tomorrow 9am",
///     "9am", "14:30"
pub fn parse_deadline_expr(s: &str, now_local: DateTime<Local>) -> Result<DateTime<Utc>> {
    let s = s.trim();
    if s.is_empty() {
        return Err(Error::Schedule("empty deadline string".into()));
    }
    if let Ok(d) = humantime::parse_duration(s) {
        let cd = ChronoDuration::from_std(d)
            .map_err(|e| Error::Schedule(format!("duration out of range: {e}")))?;
        return Ok(now_local.with_timezone(&Utc) + cd);
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Some(dt) = parse_natural(s, now_local) {
        return Ok(dt.with_timezone(&Utc));
    }
    Err(Error::Schedule(format!(
        "unrecognized deadline {s:?}; expected a duration (\"4h\"), an RFC3339 \
         timestamp, or a phrase like \"tomorrow 9am\""
    )))
}

fn parse_natural(s: &str, now: DateTime<Local>) -> Option<DateTime<Local>> {
    let lower = s.to_lowercase();
    let parts: Vec<&str> = lower.split_whitespace().collect();
    let today = now.date_naive();
    let tomorrow = today + ChronoDuration::days(1);
    match parts.as_slice() {
        ["tomorrow"] => make_local(tomorrow, NaiveTime::from_hms_opt(0, 0, 0)?),
        ["today"] => make_local(today, NaiveTime::from_hms_opt(0, 0, 0)?),
        ["tomorrow", t] => make_local(tomorrow, parse_time(t)?),
        ["today", t] => make_local(today, parse_time(t)?),
        [t] => {
            let nt = parse_time(t)?;
            let candidate = make_local(today, nt)?;
            if candidate <= now {
                make_local(tomorrow, nt)
            } else {
                Some(candidate)
            }
        }
        _ => None,
    }
}

fn make_local(date: NaiveDate, time: NaiveTime) -> Option<DateTime<Local>> {
    Local.from_local_datetime(&date.and_time(time)).single()
}

fn parse_time(s: &str) -> Option<NaiveTime> {
    let lower = s.trim().to_lowercase();
    let (body, ampm) = if let Some(rest) = lower.strip_suffix("am") {
        (rest.trim_end(), Some(false))
    } else if let Some(rest) = lower.strip_suffix("pm") {
        (rest.trim_end(), Some(true))
    } else {
        (lower.as_str(), None)
    };
    let body = body.trim();
    let (h, m): (u32, u32) = if let Some((hs, ms)) = body.split_once(':') {
        (hs.parse().ok()?, ms.parse().ok()?)
    } else {
        (body.parse().ok()?, 0)
    };
    if m >= 60 {
        return None;
    }
    let h = match ampm {
        Some(false) => {
            // AM: 12am = 00:00, 1am..11am = 1..11
            if h == 12 {
                0
            } else if h <= 11 {
                h
            } else {
                return None;
            }
        }
        Some(true) => {
            // PM: 12pm = 12:00, 1pm..11pm = 13..23
            if h == 12 {
                12
            } else if h <= 11 {
                h + 12
            } else {
                return None;
            }
        }
        None => {
            if h >= 24 {
                return None;
            }
            h
        }
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_dt(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        let date = NaiveDate::from_ymd_opt(y, mo, d).unwrap();
        let time = NaiveTime::from_hms_opt(h, mi, 0).unwrap();
        Local
            .from_local_datetime(&date.and_time(time))
            .single()
            .expect("unambiguous local time for test fixture")
    }

    #[test]
    fn parse_duration_form() {
        let now = local_dt(2026, 5, 20, 8, 0);
        let dt = parse_deadline_expr("4h", now).unwrap();
        let expected = now.with_timezone(&Utc) + ChronoDuration::hours(4);
        assert_eq!(dt, expected);
    }

    #[test]
    fn parse_duration_30m() {
        let now = local_dt(2026, 5, 20, 8, 0);
        let dt = parse_deadline_expr("30m", now).unwrap();
        let expected = now.with_timezone(&Utc) + ChronoDuration::minutes(30);
        assert_eq!(dt, expected);
    }

    #[test]
    fn parse_rfc3339_form() {
        let now = local_dt(2026, 5, 20, 8, 0);
        let dt = parse_deadline_expr("2026-05-21T09:00:00-07:00", now).unwrap();
        let expected = DateTime::parse_from_rfc3339("2026-05-21T09:00:00-07:00")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(dt, expected);
    }

    #[test]
    fn parse_natural_tomorrow_9am() {
        let now = local_dt(2026, 5, 20, 8, 0);
        let dt = parse_deadline_expr("tomorrow 9am", now).unwrap();
        let expected = local_dt(2026, 5, 21, 9, 0).with_timezone(&Utc);
        assert_eq!(dt, expected);
    }

    #[test]
    fn parse_natural_today_3pm() {
        let now = local_dt(2026, 5, 20, 8, 0);
        let dt = parse_deadline_expr("today 3pm", now).unwrap();
        let expected = local_dt(2026, 5, 20, 15, 0).with_timezone(&Utc);
        assert_eq!(dt, expected);
    }

    #[test]
    fn parse_natural_bare_9am_future() {
        let now = local_dt(2026, 5, 20, 8, 0);
        let dt = parse_deadline_expr("9am", now).unwrap();
        let expected = local_dt(2026, 5, 20, 9, 0).with_timezone(&Utc);
        assert_eq!(dt, expected);
    }

    #[test]
    fn parse_natural_bare_9am_past_rolls_to_tomorrow() {
        let now = local_dt(2026, 5, 20, 10, 0);
        let dt = parse_deadline_expr("9am", now).unwrap();
        let expected = local_dt(2026, 5, 21, 9, 0).with_timezone(&Utc);
        assert_eq!(dt, expected);
    }

    #[test]
    fn parse_natural_24h_form() {
        let now = local_dt(2026, 5, 20, 8, 0);
        let dt = parse_deadline_expr("tomorrow 14:30", now).unwrap();
        let expected = local_dt(2026, 5, 21, 14, 30).with_timezone(&Utc);
        assert_eq!(dt, expected);
    }

    #[test]
    fn parse_natural_noon_midnight() {
        let now = local_dt(2026, 5, 20, 1, 0);
        let noon = parse_deadline_expr("today 12pm", now).unwrap();
        assert_eq!(noon, local_dt(2026, 5, 20, 12, 0).with_timezone(&Utc));
        let midnight = parse_deadline_expr("today 12am", now).unwrap();
        assert_eq!(midnight, local_dt(2026, 5, 20, 0, 0).with_timezone(&Utc));
    }

    #[test]
    fn parse_empty_errs() {
        let now = local_dt(2026, 5, 20, 8, 0);
        let err = parse_deadline_expr("", now).unwrap_err();
        assert!(format!("{err}").contains("empty"));
    }

    #[test]
    fn parse_garbage_errs() {
        let now = local_dt(2026, 5, 20, 8, 0);
        let err = parse_deadline_expr("blarble", now).unwrap_err();
        assert!(format!("{err}").contains("unrecognized"));
    }

    #[test]
    fn compute_deadline_total_budget() {
        let now_local = local_dt(2026, 5, 20, 8, 0);
        let now_utc = now_local.with_timezone(&Utc);
        let sched = ConfigSchedule {
            total_budget: Some(StdDuration::from_secs(3600)),
            deadline: None,
        };
        let d = compute_deadline(&sched, now_utc, now_local).unwrap();
        assert_eq!(d.at(), now_utc + ChronoDuration::hours(1));
    }

    #[test]
    fn compute_deadline_deadline_string() {
        let now_local = local_dt(2026, 5, 20, 8, 0);
        let now_utc = now_local.with_timezone(&Utc);
        let sched = ConfigSchedule {
            total_budget: None,
            deadline: Some("tomorrow 9am".to_string()),
        };
        let d = compute_deadline(&sched, now_utc, now_local).unwrap();
        let expected = local_dt(2026, 5, 21, 9, 0).with_timezone(&Utc);
        assert_eq!(d.at(), expected);
    }

    #[test]
    fn deadline_is_expired_true_after() {
        let now = local_dt(2026, 5, 20, 8, 0).with_timezone(&Utc);
        let d = Deadline(now - ChronoDuration::seconds(1));
        assert!(d.is_expired(now));
    }

    #[test]
    fn deadline_is_expired_false_before() {
        let now = local_dt(2026, 5, 20, 8, 0).with_timezone(&Utc);
        let d = Deadline(now + ChronoDuration::seconds(60));
        assert!(!d.is_expired(now));
    }

    #[test]
    fn deadline_remaining_positive() {
        let now = local_dt(2026, 5, 20, 8, 0).with_timezone(&Utc);
        let d = Deadline(now + ChronoDuration::seconds(90));
        assert_eq!(d.remaining(now), StdDuration::from_secs(90));
    }

    #[test]
    fn deadline_remaining_saturates_zero() {
        let now = local_dt(2026, 5, 20, 8, 0).with_timezone(&Utc);
        let d = Deadline(now - ChronoDuration::seconds(5));
        assert_eq!(d.remaining(now), StdDuration::ZERO);
    }
}
