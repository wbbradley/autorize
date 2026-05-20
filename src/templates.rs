const CONFIG_TMPL: &str = include_str!("templates/config.toml.tmpl");
const PROGRAM_TMPL: &str = include_str!("templates/program.md.tmpl");

pub fn render_config(experiment_name: &str) -> String {
    CONFIG_TMPL.replace("{{experiment_name}}", experiment_name)
}

pub fn render_program(experiment_name: &str) -> String {
    PROGRAM_TMPL.replace("{{experiment_name}}", experiment_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn renders_experiment_name() {
        let rendered = render_config("pi");
        assert!(rendered.contains("name = \"pi\""));
        assert!(!rendered.contains("{{experiment_name}}"));
    }

    #[test]
    fn renders_program_experiment_name() {
        let rendered = render_program("pi");
        assert!(rendered.contains("# pi"));
        assert!(!rendered.contains("{{experiment_name}}"));
    }

    #[test]
    fn template_is_valid_config() {
        let rendered = render_config("pi");
        let cfg = Config::from_toml(&rendered).expect("template parses");
        assert_eq!(cfg.experiment.name, "pi");
    }
}
