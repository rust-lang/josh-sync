use anyhow::Context;
use std::path::Path;

#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "kebab-case")]
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
    /// Operation(s) that should be performed after a pull.
    /// Can be used to post-process the state of the repository after a pull happens.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_pull: Vec<PostPullOperation>,
}

/// Execute an operation after a pull, and if something changes in the local git state,
/// perform a commit.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct PostPullOperation {
    /// Execute a command with these arguments
    /// At least one argument has to be present.
    /// You can run e.g. bash if you want to do more complicated stuff.
    pub cmd: Vec<String>,
    /// If the git state has changed after `cmd`, add all changes to the index (`git add -u`)
    /// and create a commit with the following commit message.
    pub commit_message: String,
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
