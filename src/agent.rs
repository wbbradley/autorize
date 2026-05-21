use std::{collections::BTreeMap, fs, path::Path, time::Duration};

use crate::{
    config::AgentStdin,
    error::{Error, Result},
    subproc::{self, CommandOutput},
};

pub struct AgentSpec<'a> {
    pub command_template: &'a str,
    pub prompt_file: &'a Path,
    pub workdir: &'a Path,
    pub iter: u64,
    pub budget: Duration,
    pub workdir_var: &'a str,
    pub env: &'a BTreeMap<String, String>,
    pub stdin: AgentStdin,
}

#[derive(Debug)]
pub struct AgentOutput {
    pub exit_code: Option<i32>,
    pub killed_by_budget: bool,
    pub stdout: String,
    pub stderr: String,
}

pub fn run_agent(spec: &AgentSpec) -> Result<AgentOutput> {
    let command = substitute(
        spec.command_template,
        spec.prompt_file,
        spec.workdir,
        spec.iter,
    )?;
    let mut env = expand_env(spec.env);
    env.insert(spec.workdir_var.to_string(), path_to_string(spec.workdir)?);
    let stdin_payload = match spec.stdin {
        AgentStdin::None => None,
        AgentStdin::Prompt => Some(fs::read(spec.prompt_file)?),
    };
    let out: CommandOutput =
        subproc::run_command_with_budget(&command, spec.workdir, spec.budget, &env, stdin_payload)?;
    Ok(AgentOutput {
        exit_code: out.exit_code,
        killed_by_budget: out.timed_out,
        stdout: out.stdout,
        stderr: out.stderr,
    })
}

fn substitute(tmpl: &str, prompt: &Path, workdir: &Path, iter: u64) -> Result<String> {
    let prompt_s = path_to_string(prompt)?;
    let workdir_s = path_to_string(workdir)?;
    Ok(tmpl
        .replace("{prompt_file}", &prompt_s)
        .replace("{workdir}", &workdir_s)
        .replace("{iter}", &iter.to_string()))
}

fn path_to_string(p: &Path) -> Result<String> {
    p.to_str()
        .map(str::to_string)
        .ok_or_else(|| Error::Subproc(format!("path {p:?} is not valid UTF-8")))
}

/// Expand `$NAME` and `${NAME}` references in each value against the
/// parent process environment. Unset vars expand to the empty string.
/// A literal `$` followed by a non-name char is preserved.
fn expand_env(env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    env.iter()
        .map(|(k, v)| (k.clone(), expand_one(v)))
        .collect()
}

fn expand_one(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            // ${NAME}
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                if let Some(rel_end) = bytes[i + 2..].iter().position(|&b| b == b'}') {
                    let name = &s[i + 2..i + 2 + rel_end];
                    if is_valid_name(name) {
                        out.push_str(&std::env::var(name).unwrap_or_default());
                        i = i + 2 + rel_end + 1;
                        continue;
                    }
                }
                // unterminated or invalid — preserve `$` literal, walk past it.
                out.push('$');
                i += 1;
                continue;
            }
            // $NAME
            let start = i + 1;
            let mut end = start;
            if end < bytes.len() && (bytes[end].is_ascii_alphabetic() || bytes[end] == b'_') {
                end += 1;
                while end < bytes.len()
                    && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
                {
                    end += 1;
                }
                let name = &s[start..end];
                out.push_str(&std::env::var(name).unwrap_or_default());
                i = end;
                continue;
            }
            // Literal `$` (followed by non-name char or end of string).
            out.push('$');
            i += 1;
        } else {
            // Copy the longest run of non-`$` bytes in one shot. Safe
            // because `$` is a single-byte ASCII codepoint, so the slice
            // boundaries fall on UTF-8 boundaries.
            let next = bytes[i..]
                .iter()
                .position(|&b| b == b'$')
                .map(|p| i + p)
                .unwrap_or(bytes.len());
            out.push_str(&s[i..next]);
            i = next;
        }
    }
    out
}

fn is_valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Instant};

    use tempfile::tempdir;

    use super::*;

    fn empty_env() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    #[test]
    fn substitute_replaces_all_placeholders() {
        let p = PathBuf::from("/tmp/p.md");
        let w = PathBuf::from("/tmp/wd");
        let out = substitute(
            "agent --in {prompt_file} --wd {workdir} --iter {iter}",
            &p,
            &w,
            7,
        )
        .unwrap();
        assert_eq!(out, "agent --in /tmp/p.md --wd /tmp/wd --iter 7");
    }

    #[test]
    fn expand_env_passthrough_simple() {
        let var = "AUTORIZE_TEST_EXP_PASS";
        // SAFETY: tests use distinct var names per test, so no thread races.
        unsafe { std::env::set_var(var, "bar") };
        let mut env = BTreeMap::new();
        env.insert("X".to_string(), format!("${var}"));
        let out = expand_env(&env);
        assert_eq!(out.get("X").map(String::as_str), Some("bar"));
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn expand_env_braced_form() {
        let var = "AUTORIZE_TEST_EXP_BRACED";
        unsafe { std::env::set_var(var, "baz") };
        let mut env = BTreeMap::new();
        env.insert("X".to_string(), format!("${{{var}}}"));
        let out = expand_env(&env);
        assert_eq!(out.get("X").map(String::as_str), Some("baz"));
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn expand_env_missing_var_to_empty() {
        let var = "AUTORIZE_TEST_EXP_MISSING_DEF_NOT_SET";
        // Ensure unset.
        unsafe { std::env::remove_var(var) };
        let mut env = BTreeMap::new();
        env.insert("X".to_string(), format!("${var}"));
        let out = expand_env(&env);
        assert_eq!(out.get("X").map(String::as_str), Some(""));
    }

    #[test]
    fn expand_env_preserves_literal_text() {
        let var = "AUTORIZE_TEST_EXP_LITERAL";
        unsafe { std::env::set_var(var, "bar") };
        let mut env = BTreeMap::new();
        env.insert("X".to_string(), format!("prefix-${var}-suffix"));
        let out = expand_env(&env);
        assert_eq!(out.get("X").map(String::as_str), Some("prefix-bar-suffix"));
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn expand_env_no_expansion_when_no_dollar() {
        let mut env = BTreeMap::new();
        env.insert("X".to_string(), "literal".to_string());
        let out = expand_env(&env);
        assert_eq!(out.get("X").map(String::as_str), Some("literal"));
    }

    #[test]
    fn expand_env_preserves_dollar_followed_by_non_name() {
        let mut env = BTreeMap::new();
        env.insert("X".to_string(), "price $5".to_string());
        let out = expand_env(&env);
        assert_eq!(out.get("X").map(String::as_str), Some("price $5"));
    }

    #[test]
    fn run_agent_prompt_file_mode() {
        let dir = tempdir().unwrap();
        let prompt = dir.path().join("prompt.md");
        fs::write(&prompt, "prompt-body\n").unwrap();
        let env = empty_env();
        let spec = AgentSpec {
            command_template: "cat {prompt_file}",
            prompt_file: &prompt,
            workdir: dir.path(),
            iter: 0,
            budget: Duration::from_secs(5),
            workdir_var: "AUTORIZE_WORKDIR",
            env: &env,
            stdin: AgentStdin::None,
        };
        let out = run_agent(&spec).unwrap();
        assert!(!out.killed_by_budget);
        assert_eq!(out.stdout, "prompt-body\n");
    }

    #[test]
    fn run_agent_stdin_prompt_mode() {
        let dir = tempdir().unwrap();
        let prompt = dir.path().join("prompt.md");
        fs::write(&prompt, "via-stdin\n").unwrap();
        let env = empty_env();
        let spec = AgentSpec {
            command_template: "cat",
            prompt_file: &prompt,
            workdir: dir.path(),
            iter: 0,
            budget: Duration::from_secs(5),
            workdir_var: "AUTORIZE_WORKDIR",
            env: &env,
            stdin: AgentStdin::Prompt,
        };
        let out = run_agent(&spec).unwrap();
        assert!(!out.killed_by_budget);
        assert_eq!(out.stdout, "via-stdin\n");
    }

    #[test]
    fn run_agent_sets_workdir_var() {
        let dir = tempdir().unwrap();
        let prompt = dir.path().join("prompt.md");
        fs::write(&prompt, "x").unwrap();
        let env = empty_env();
        let spec = AgentSpec {
            command_template: "echo $AUTORIZE_WORKDIR",
            prompt_file: &prompt,
            workdir: dir.path(),
            iter: 0,
            budget: Duration::from_secs(5),
            workdir_var: "AUTORIZE_WORKDIR",
            env: &env,
            stdin: AgentStdin::Prompt,
        };
        let out = run_agent(&spec).unwrap();
        assert_eq!(out.stdout.trim(), dir.path().to_str().unwrap());
    }

    #[test]
    fn run_agent_iter_substituted() {
        let dir = tempdir().unwrap();
        let prompt = dir.path().join("prompt.md");
        fs::write(&prompt, "x").unwrap();
        let env = empty_env();
        let spec = AgentSpec {
            command_template: "echo {iter}",
            prompt_file: &prompt,
            workdir: dir.path(),
            iter: 42,
            budget: Duration::from_secs(5),
            workdir_var: "AUTORIZE_WORKDIR",
            env: &env,
            stdin: AgentStdin::Prompt,
        };
        let out = run_agent(&spec).unwrap();
        assert_eq!(out.stdout, "42\n");
    }

    #[test]
    fn run_agent_kills_long_running() {
        let dir = tempdir().unwrap();
        let prompt = dir.path().join("prompt.md");
        fs::write(&prompt, "x").unwrap();
        let env = empty_env();
        let spec = AgentSpec {
            command_template: "sleep 30",
            prompt_file: &prompt,
            workdir: dir.path(),
            iter: 0,
            budget: Duration::from_secs(1),
            workdir_var: "AUTORIZE_WORKDIR",
            env: &env,
            stdin: AgentStdin::Prompt,
        };
        let started = Instant::now();
        let out = run_agent(&spec).unwrap();
        let elapsed = started.elapsed();
        assert!(out.killed_by_budget, "expected killed_by_budget: {out:?}");
        assert!(elapsed < Duration::from_secs(8), "took: {elapsed:?}");
    }
}
