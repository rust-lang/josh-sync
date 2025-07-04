use anyhow::Context;
use std::path::Path;

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct JoshConfig {
    #[serde(default = "default_org")]
    pub org: String,
    pub repo: String,
    /// Relative path where the subtree is located in rust-lang/rust.
    /// For example `src/doc/rustc-dev-guide`.
    pub path: Option<String>,
    /// Optional filter specification for Josh.
    /// It cannot be used together with `path`.
    pub filter: Option<String>,
}

impl JoshConfig {
    pub fn full_repo_name(&self) -> String {
        format!("{}/{}", self.org, self.repo)
    }

    pub fn construct_josh_filter(&self) -> String {
        match (&self.path, &self.filter) {
            (Some(path), None) => format!(":/{path}"),
            (None, Some(filter)) => filter.clone(),
            _ => unreachable!("Config contains both path and a filter"),
        }
    }

    pub fn write(&self, path: &Path) -> anyhow::Result<()> {
        let config = toml::to_string_pretty(self).context("cannot serialize config")?;
        std::fs::write(path, config).context("cannot write config")?;
        Ok(())
    }
}

fn default_org() -> String {
    String::from("rust-lang")
}

pub fn load_config(path: &Path) -> anyhow::Result<JoshConfig> {
    let data = std::fs::read_to_string(path)
        .with_context(|| format!("cannot load config file from {}", path.display()))?;
    let config: JoshConfig = toml::from_str(&data).context("cannot load config as TOML")?;
    if config.path.is_some() == config.filter.is_some() {
        return if config.path.is_some() {
            Err(anyhow::anyhow!("Cannot specify both `path` and `filter`"))
        } else {
            Err(anyhow::anyhow!("Must specify one of `path` and `filter`"))
        };
    }

    Ok(config)
}
