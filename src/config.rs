use std::{collections::BTreeMap, time::Duration};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub experiment: Experiment,
    pub objective: Objective,
    #[serde(default)]
    pub boundaries: Boundaries,
    #[serde(default)]
    pub setup: Setup,
    #[serde(default)]
    pub teardown: Teardown,
    #[serde(default)]
    pub iteration: Iteration,
    pub schedule: Schedule,
    pub agent: Agent,
    #[serde(default)]
    pub summarize: Summarize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Experiment {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Objective {
    pub command: String,
    pub direction: Direction,
    pub parse: ParseSpec,
    #[serde(with = "humantime_serde", default = "default_objective_timeout")]
    pub timeout: Duration,
    #[serde(default)]
    pub fail_mode: FailMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Min,
    Max,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FailMode {
    #[default]
    Invalid,
    Worst,
    Abort,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ParseSpec {
    Float,
    Regex { pattern: String },
    Jq { path: String },
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Boundaries {
    #[serde(default)]
    pub allow_paths: Vec<String>,
    #[serde(default)]
    pub deny_paths: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Setup {
    #[serde(default)]
    pub command: String,
    #[serde(with = "humantime_serde", default = "default_setup_timeout")]
    pub timeout: Duration,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            command: String::new(),
            timeout: default_setup_timeout(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Teardown {
    #[serde(default)]
    pub command: String,
    #[serde(with = "humantime_serde", default = "default_teardown_timeout")]
    pub timeout: Duration,
}

impl Default for Teardown {
    fn default() -> Self {
        Self {
            command: String::new(),
            timeout: default_teardown_timeout(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Iteration {
    #[serde(with = "humantime_serde", default = "default_iter_budget")]
    pub budget: Duration,
    #[serde(default)]
    pub max_iterations: u64,
    #[serde(default)]
    pub keep_worktrees: bool,
    #[serde(default = "default_max_consecutive_noops")]
    pub max_consecutive_noops: u32,
}

impl Default for Iteration {
    fn default() -> Self {
        Self {
            budget: default_iter_budget(),
            max_iterations: 0,
            keep_worktrees: false,
            max_consecutive_noops: default_max_consecutive_noops(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Schedule {
    #[serde(default, with = "humantime_serde::option")]
    pub total_budget: Option<Duration>,
    #[serde(default)]
    pub deadline: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Agent {
    pub command: String,
    #[serde(default = "default_workdir_var")]
    pub workdir_var: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub stdin: AgentStdin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentStdin {
    #[default]
    None,
    Prompt,
}

/// Post-iteration summarization. After the worker agent exits, autorize can
/// run a SEPARATE (typically weaker/cheaper) model to write a 1-2 sentence
/// summary of what the iteration attempted and why the score moved. It has its
/// own command and timeout (independent of `iteration.budget`), mirrors
/// `[agent]`'s `{prompt_file}`/`{workdir}`/`{iter}` substitution and `stdin`
/// modes, and is best-effort: any failure leaves the summary empty without
/// affecting the iteration outcome. Disabled when the section is absent.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Summarize {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub command: String,
    #[serde(with = "humantime_serde", default = "default_summarize_timeout")]
    pub timeout: Duration,
    #[serde(default)]
    pub stdin: AgentStdin,
}

impl Default for Summarize {
    fn default() -> Self {
        Self {
            enabled: false,
            command: String::new(),
            timeout: default_summarize_timeout(),
            stdin: AgentStdin::None,
        }
    }
}

fn default_objective_timeout() -> Duration {
    Duration::from_secs(60)
}

fn default_setup_timeout() -> Duration {
    Duration::from_secs(5 * 60)
}

fn default_teardown_timeout() -> Duration {
    Duration::from_secs(60)
}

fn default_summarize_timeout() -> Duration {
    Duration::from_secs(60)
}

fn default_iter_budget() -> Duration {
    Duration::from_secs(5 * 60)
}

fn default_max_consecutive_noops() -> u32 {
    5
}

fn default_workdir_var() -> String {
    "AUTORIZE_WORKDIR".to_string()
}

fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl Config {
    pub fn from_toml(s: &str) -> Result<Config> {
        let c: Config = toml::from_str(s)?;
        c.validate()?;
        Ok(c)
    }

    pub fn validate(&self) -> Result<()> {
        if !is_valid_name(&self.experiment.name) {
            return Err(Error::Config(format!(
                "experiment.name {:?} must match [A-Za-z0-9_-]+ and be non-empty",
                self.experiment.name
            )));
        }

        match (
            self.schedule.total_budget.is_some(),
            self.schedule.deadline.is_some(),
        ) {
            (true, true) => {
                return Err(Error::Config(
                    "schedule: set exactly one of `total_budget` or `deadline`, not both"
                        .to_string(),
                ));
            }
            (false, false) => {
                return Err(Error::Config(
                    "schedule: one of `total_budget` or `deadline` is required".to_string(),
                ));
            }
            _ => {}
        }

        if self.iteration.budget.is_zero() {
            return Err(Error::Config(
                "iteration.budget must be greater than zero".to_string(),
            ));
        }

        if self.agent.command.trim().is_empty() {
            return Err(Error::Config("agent.command must be non-empty".to_string()));
        }

        if matches!(self.agent.stdin, AgentStdin::None)
            && !self.agent.command.contains("{prompt_file}")
        {
            return Err(Error::Config(
                "agent.command must contain `{prompt_file}` when agent.stdin is \"none\""
                    .to_string(),
            ));
        }

        if self.objective.command.trim().is_empty() {
            return Err(Error::Config(
                "objective.command must be non-empty".to_string(),
            ));
        }

        if let ParseSpec::Regex { pattern } = &self.objective.parse
            && pattern.is_empty()
        {
            return Err(Error::Config(
                "objective.parse regex pattern must be non-empty".to_string(),
            ));
        }
        if let ParseSpec::Jq { path } = &self.objective.parse
            && path.is_empty()
        {
            return Err(Error::Config(
                "objective.parse jq path must be non-empty".to_string(),
            ));
        }

        if self.summarize.enabled {
            if self.summarize.command.trim().is_empty() {
                return Err(Error::Config(
                    "summarize.command must be non-empty when summarize.enabled is true"
                        .to_string(),
                ));
            }
            if matches!(self.summarize.stdin, AgentStdin::None)
                && !self.summarize.command.contains("{prompt_file}")
            {
                return Err(Error::Config(
                    "summarize.command must contain `{prompt_file}` when summarize.stdin is \"none\""
                        .to_string(),
                ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::render_config;

    fn base_toml() -> String {
        r#"
[experiment]
name = "pi"

[objective]
command = "bash score.sh"
direction = "min"
parse = { kind = "float" }

[schedule]
total_budget = "4h"

[agent]
command = "claude --print {prompt_file}"
"#
        .to_string()
    }

    #[test]
    fn parses_full_default_template() {
        let s = render_config("foo");
        let cfg = Config::from_toml(&s).expect("default template should parse");
        assert_eq!(cfg.experiment.name, "foo");
        assert_eq!(cfg.objective.direction, Direction::Min);
        assert!(matches!(cfg.objective.parse, ParseSpec::Float));
        assert_eq!(cfg.objective.fail_mode, FailMode::Invalid);
        assert_eq!(cfg.iteration.max_consecutive_noops, 5);
        assert_eq!(cfg.iteration.budget, Duration::from_secs(300));
        assert!(cfg.schedule.total_budget.is_some());
        assert!(cfg.schedule.deadline.is_none());
    }

    #[test]
    fn parses_float_variant() {
        let cfg = Config::from_toml(&base_toml()).unwrap();
        assert!(matches!(cfg.objective.parse, ParseSpec::Float));
    }

    #[test]
    fn parses_regex_variant() {
        let s = base_toml().replace(
            "parse = { kind = \"float\" }",
            "parse = { kind = \"regex\", pattern = \"score=([0-9.]+)\" }",
        );
        let cfg = Config::from_toml(&s).unwrap();
        match cfg.objective.parse {
            ParseSpec::Regex { pattern } => assert_eq!(pattern, "score=([0-9.]+)"),
            _ => panic!("expected regex"),
        }
    }

    #[test]
    fn parses_jq_variant() {
        let s = base_toml().replace(
            "parse = { kind = \"float\" }",
            "parse = { kind = \"jq\", path = \".metrics.bpb\" }",
        );
        let cfg = Config::from_toml(&s).unwrap();
        match cfg.objective.parse {
            ParseSpec::Jq { path } => assert_eq!(path, ".metrics.bpb"),
            _ => panic!("expected jq"),
        }
    }

    #[test]
    fn parses_total_budget_schedule() {
        let cfg = Config::from_toml(&base_toml()).unwrap();
        assert_eq!(
            cfg.schedule.total_budget,
            Some(Duration::from_secs(4 * 3600))
        );
        assert!(cfg.schedule.deadline.is_none());
    }

    #[test]
    fn parses_deadline_schedule() {
        let s = base_toml().replace(
            "total_budget = \"4h\"",
            "deadline = \"2026-05-21T09:00:00-07:00\"",
        );
        let cfg = Config::from_toml(&s).unwrap();
        assert!(cfg.schedule.total_budget.is_none());
        assert_eq!(
            cfg.schedule.deadline.as_deref(),
            Some("2026-05-21T09:00:00-07:00")
        );
    }

    #[test]
    fn rejects_both_schedule_fields_set() {
        let s = base_toml().replace(
            "total_budget = \"4h\"",
            "total_budget = \"4h\"\ndeadline = \"2026-05-21T09:00:00-07:00\"",
        );
        let err = Config::from_toml(&s).unwrap_err();
        assert!(format!("{err}").contains("exactly one"), "got: {err}");
    }

    #[test]
    fn rejects_neither_schedule_field_set() {
        let s = base_toml().replace("total_budget = \"4h\"", "");
        let err = Config::from_toml(&s).unwrap_err();
        assert!(format!("{err}").contains("required"), "got: {err}");
    }

    #[test]
    fn rejects_bad_direction() {
        let s = base_toml().replace("direction = \"min\"", "direction = \"minimize\"");
        Config::from_toml(&s).unwrap_err();
    }

    #[test]
    fn rejects_bad_fail_mode() {
        let s = base_toml().replace(
            "parse = { kind = \"float\" }",
            "parse = { kind = \"float\" }\nfail_mode = \"explode\"",
        );
        Config::from_toml(&s).unwrap_err();
    }

    #[test]
    fn rejects_empty_name() {
        let s = base_toml().replace("name = \"pi\"", "name = \"\"");
        let err = Config::from_toml(&s).unwrap_err();
        assert!(format!("{err}").contains("name"));
    }

    #[test]
    fn rejects_name_with_slash() {
        let s = base_toml().replace("name = \"pi\"", "name = \"../etc\"");
        let err = Config::from_toml(&s).unwrap_err();
        assert!(format!("{err}").contains("name"));
    }

    #[test]
    fn requires_prompt_file_placeholder_when_stdin_none() {
        let s = base_toml().replace(
            "command = \"claude --print {prompt_file}\"",
            "command = \"claude --print\"",
        );
        let err = Config::from_toml(&s).unwrap_err();
        assert!(format!("{err}").contains("prompt_file"), "got: {err}");
    }

    #[test]
    fn accepts_stdin_prompt_without_placeholder() {
        let s = base_toml().replace(
            "command = \"claude --print {prompt_file}\"",
            "command = \"claude --print -\"\nstdin = \"prompt\"",
        );
        let cfg = Config::from_toml(&s).unwrap();
        assert_eq!(cfg.agent.stdin, AgentStdin::Prompt);
        assert!(!cfg.agent.command.contains("{prompt_file}"));
    }

    #[test]
    fn parses_5m_budget() {
        let cfg = Config::from_toml(&base_toml()).unwrap();
        assert_eq!(cfg.iteration.budget, Duration::from_secs(300));
    }

    #[test]
    fn summarize_disabled_by_default_when_section_absent() {
        // base_toml has no [summarize] — it must default to disabled, which is
        // the back-compatible no-op behavior for pre-existing experiments.
        let cfg = Config::from_toml(&base_toml()).unwrap();
        assert!(!cfg.summarize.enabled);
        assert_eq!(cfg.summarize.timeout, Duration::from_secs(60));
        assert_eq!(cfg.summarize.stdin, AgentStdin::None);
    }

    #[test]
    fn parses_summarize_section() {
        let s = format!(
            "{}\n[summarize]\nenabled = true\ncommand = \"claude --model haiku --print {{prompt_file}}\"\ntimeout = \"30s\"\nstdin = \"none\"\n",
            base_toml()
        );
        let cfg = Config::from_toml(&s).unwrap();
        assert!(cfg.summarize.enabled);
        assert_eq!(cfg.summarize.timeout, Duration::from_secs(30));
        assert_eq!(cfg.summarize.stdin, AgentStdin::None);
        assert!(cfg.summarize.command.contains("haiku"));
    }

    #[test]
    fn default_template_enables_summarize() {
        let cfg = Config::from_toml(&render_config("foo")).unwrap();
        assert!(cfg.summarize.enabled);
        assert!(cfg.summarize.command.contains("{prompt_file}"));
    }

    #[test]
    fn rejects_enabled_summarize_without_command() {
        let s = format!("{}\n[summarize]\nenabled = true\n", base_toml());
        let err = Config::from_toml(&s).unwrap_err();
        assert!(format!("{err}").contains("summarize.command"), "got: {err}");
    }

    #[test]
    fn rejects_summarize_stdin_none_without_placeholder() {
        let s = format!(
            "{}\n[summarize]\nenabled = true\ncommand = \"claude --print\"\nstdin = \"none\"\n",
            base_toml()
        );
        let err = Config::from_toml(&s).unwrap_err();
        assert!(format!("{err}").contains("prompt_file"), "got: {err}");
    }

    #[test]
    fn accepts_summarize_stdin_prompt_without_placeholder() {
        let s = format!(
            "{}\n[summarize]\nenabled = true\ncommand = \"claude --print -\"\nstdin = \"prompt\"\n",
            base_toml()
        );
        let cfg = Config::from_toml(&s).unwrap();
        assert_eq!(cfg.summarize.stdin, AgentStdin::Prompt);
    }
}
