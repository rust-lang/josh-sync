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
    /// Optional subtree filter applied to the local `HEAD` during round-trip check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtree_filter: Option<String>,
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
        let filter = match (&self.path, &self.filter) {
            (Some(path), None) => format!(":/{path}"),
            (None, Some(filter)) => filter.clone(),
            _ => unreachable!("Config contains both path and a filter"),
        };

        let filter = convert_rev_syntax(&filter);
        let filter = wrap_compat(&filter);
        filter
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

/// Converts filters from old `:rev(sha:filter)` syntax to new
/// `:rev(<=sha:filter)` syntax. Null SHAs (40 zeros) become `_`.
/// Only touches SHAs inside `:rev(...)` blocks.
fn convert_rev_syntax(input: &str) -> String {
    let rev_block = regex::Regex::new(r":rev\([^)]*\)").unwrap();
    let entry = regex::Regex::new(
        r"(?x)
        ([,(])                # delimiter before entry
        (0{40}|[0-9a-f]{40})  # full SHA
        :                     # colon separator
    ",
    )
    .unwrap();

    rev_block
        .replace_all(input, |block: &regex::Captures| {
            entry
                .replace_all(&block[0], |caps: &regex::Captures| {
                    let delim = &caps[1];
                    let sha = &caps[2];
                    if sha.chars().all(|c| c == '0') {
                        format!("{delim}_:")
                    } else {
                        format!("{delim}<={sha}:")
                    }
                })
                .into_owned()
        })
        .into_owned()
}

/// Wraps a filter with the backwards compatibility meta options for
/// trivial merge preservation and CRLF normalization in gpgsig headers.
///
/// `:your/filter` becomes
/// `:~(history="keep-trivial-merges",gpgsig="norm-lf")[:your/filter]`
fn wrap_compat(filter: &str) -> String {
    format!(":~(history=\"keep-trivial-merges\",gpgsig=\"norm-lf\")[{filter}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_rev_block_unchanged() {
        assert_eq!(convert_rev_syntax(":/some/path"), ":/some/path");
    }

    #[test]
    fn single_sha_gets_prefix() {
        assert_eq!(
            convert_rev_syntax(":rev(3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/some/path)"),
            ":rev(<=3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/some/path)",
        );
    }

    #[test]
    fn null_sha_becomes_underscore() {
        assert_eq!(
            convert_rev_syntax(":rev(0000000000000000000000000000000000000000:/some/path)"),
            ":rev(_:/some/path)",
        );
    }

    #[test]
    fn multiple_entries_in_rev_block() {
        assert_eq!(
            convert_rev_syntax(
                ":rev(3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/p1,\
                 e4c7a2d8f1b3e5a9d6c0f2b4a7e1d3c5f8a0b6e9:/p2,\
                 0000000000000000000000000000000000000000:/p3)"
            ),
            ":rev(<=3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/p1,\
             <=e4c7a2d8f1b3e5a9d6c0f2b4a7e1d3c5f8a0b6e9:/p2,\
             _:/p3)",
        );
    }

    #[test]
    fn already_converted_syntax_unchanged() {
        assert_eq!(
            convert_rev_syntax(":rev(<=3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/some/path)"),
            ":rev(<=3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/some/path)",
        );
    }

    #[test]
    fn underscore_syntax_unchanged() {
        assert_eq!(
            convert_rev_syntax(":rev(_:/some/path)"),
            ":rev(_:/some/path)",
        );
    }

    #[test]
    fn sha_outside_rev_block_unchanged() {
        assert_eq!(
            convert_rev_syntax("3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/some/path"),
            "3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/some/path",
        );
    }

    #[test]
    fn wrap_compat_simple_filter() {
        assert_eq!(
            wrap_compat(":/some/path"),
            ":~(history=\"keep-trivial-merges\",gpgsig=\"norm-lf\")[:/some/path]",
        );
    }

    #[test]
    fn wrap_compat_rev_filter() {
        assert_eq!(
            wrap_compat(
                ":rev(75dd959a3a40eb5b4574f8d2e23aa6efbeb33573:prefix=src/tools/miri):/src/tools/miri"
            ),
            ":~(history=\"keep-trivial-merges\",gpgsig=\"norm-lf\")\
             [:rev(75dd959a3a40eb5b4574f8d2e23aa6efbeb33573:prefix=src/tools/miri):/src/tools/miri]",
        );
    }

    #[test]
    fn multiple_rev_blocks() {
        assert_eq!(
            convert_rev_syntax(
                ":rev(3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/p1)\
                 :rev(e4c7a2d8f1b3e5a9d6c0f2b4a7e1d3c5f8a0b6e9:/p2)"
            ),
            ":rev(<=3a1f5e2b9c8d4e7f6a0b1c2d3e4f5a6b7c8d9e0f:/p1)\
             :rev(<=e4c7a2d8f1b3e5a9d6c0f2b4a7e1d3c5f8a0b6e9:/p2)",
        );
    }
}
