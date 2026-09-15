use crate::sync::{FilterVersion, PullMode};
use anyhow::Context;
use std::path::{Path, PathBuf};

const DEFAULT_RUST_VERSION_PATH: &str = "rust-version";
const DEFAULT_RUST_TOOLCHAIN_PATH: &str = "rust-toolchain.toml";

#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct JoshConfig {
    #[serde(default = "default_org")]
    pub org: String,
    /// Name of the subtree repository. For example `rustc-dev-guide`.
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
    /// Optional subtree filter applied to the local `HEAD` during round-trip check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtree_filter: Option<String>,
    /// Optional filter version that determines which post-processing will be applied to the
    /// specified filter.
    ///
    /// This exists for backwards compatibility with repositories using an older filter syntax.
    #[serde(
        default = "default_filter_version",
        skip_serializing_if = "skip_serializing_filter_version",
        with = "filter_version"
    )]
    pub filter_version: FilterVersion,
    /// What to use as the base commit to sync from. Defaults to the latest commit of the remote
    /// repository.
    #[serde(default)]
    pub pull_mode: PullMode,
}

impl JoshConfig {
    pub fn full_repo_name(&self) -> String {
        format!("{}/{}", self.org, self.repo)
    }

    pub fn write(&self, path: &Path) -> anyhow::Result<()> {
        let config = toml::to_string_pretty(self).context("cannot serialize config")?;
        std::fs::write(path, config).context("cannot write config")?;
        Ok(())
    }

    pub fn rust_version_path(&self) -> PathBuf {
        match self.pull_mode {
            PullMode::Latest => PathBuf::from(DEFAULT_RUST_VERSION_PATH),
            PullMode::Nightly => PathBuf::from(DEFAULT_RUST_TOOLCHAIN_PATH),
        }
    }
}

mod filter_version {
    use crate::sync::FilterVersion;
    use serde::de::{Error, Unexpected};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(
        version: &FilterVersion,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let num: u32 = match version {
            FilterVersion::Version1 => 1,
            FilterVersion::Version2 => 2,
        };
        num.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<FilterVersion, D::Error> {
        let num = u32::deserialize(deserializer)?;
        match num {
            1 => Ok(FilterVersion::Version1),
            2 => Ok(FilterVersion::Version2),
            v => Err(D::Error::invalid_value(
                Unexpected::Unsigned(v as u64),
                &"1 or 2",
            )),
        }
    }
}

fn default_filter_version() -> FilterVersion {
    FilterVersion::Version1
}

fn skip_serializing_filter_version(version: &FilterVersion) -> bool {
    match version {
        FilterVersion::Version1 => true,
        FilterVersion::Version2 => false,
    }
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
