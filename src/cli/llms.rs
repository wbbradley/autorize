const LLMS_MD: &str = include_str!("../llms.md");

#[derive(clap::Args, Debug)]
/// Print an exhaustive markdown reference aimed at LLM/agent consumers.
///
/// Use this when dropping an agent into a fresh repo: `autorize llms`
/// prints everything an agent needs to drive `init` → edit config →
/// `run` → `status`/`resume` without reading source.
pub struct LlmsArgs {}

pub fn run(_args: LlmsArgs) -> anyhow::Result<()> {
    print!("{LLMS_MD}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn collect_keys(v: &toml::Value, into: &mut BTreeSet<String>) {
        if let toml::Value::Table(t) = v {
            for (k, sub) in t {
                into.insert(k.clone());
                collect_keys(sub, into);
            }
        }
    }

    #[test]
    fn llms_doc_mentions_every_config_field() {
        let tmpl = crate::templates::render_config("test");
        let value: toml::Value = toml::from_str(&tmpl).unwrap();
        let mut names = BTreeSet::new();
        collect_keys(&value, &mut names);
        let missing: Vec<_> = names
            .iter()
            .filter(|k| !LLMS_MD.contains(k.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "llms.md is missing config keys: {missing:?}"
        );
    }

    #[test]
    fn prints_non_empty() {
        assert!(!LLMS_MD.is_empty());
        assert!(
            LLMS_MD.trim_start().starts_with('#'),
            "expected leading markdown heading"
        );
    }

    #[test]
    fn mentions_all_outcomes() {
        for outcome in [
            "\"merged\"",
            "\"discarded\"",
            "\"noop\"",
            "\"invalid\"",
            "\"killed\"",
            "\"denied\"",
        ] {
            assert!(LLMS_MD.contains(outcome), "missing outcome {outcome}");
        }
    }

    #[test]
    fn mentions_all_parse_kinds_and_fail_modes() {
        for token in [
            "\"float\"",
            "\"regex\"",
            "\"jq\"",
            "\"invalid\"",
            "\"worst\"",
            "\"abort\"",
        ] {
            assert!(LLMS_MD.contains(token), "missing token {token}");
        }
    }

    #[test]
    fn mentions_all_subcommands() {
        for sub in [
            "autorize init",
            "autorize run",
            "autorize status",
            "autorize resume",
            "autorize llms",
        ] {
            assert!(LLMS_MD.contains(sub), "missing subcommand {sub}");
        }
    }

    #[test]
    fn mentions_all_current_steps() {
        for step in [
            "Idle",
            "AllocateIter",
            "CreateWorktree",
            "RunSetup",
            "BuildPrompt",
            "InvokeAgent",
            "CaptureDiff",
            "RunTeardown",
            "Score",
            "Decide",
            "Merge",
            "Discard",
            "Cleanup",
            "Record",
            "CheckDeadline",
            "Done",
        ] {
            assert!(LLMS_MD.contains(step), "missing CurrentStep {step}");
        }
    }
}
