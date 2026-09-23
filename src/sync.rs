use crate::SyncContext;
use crate::config::{JoshConfig, PostPullOperation};
use crate::josh::{JoshFilter, JoshProxy, try_install_josh_filter};
use crate::utils::{ensure_clean_git_state, nightly_date_to_sha, prompt};
use crate::utils::{get_current_head_sha, run_command_at};
use crate::utils::{run_command, stream_command};
use anyhow::{Context, Error};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use toml_edit::{Document, DocumentMut};

pub const DEFAULT_UPSTREAM_REPO: &str = "rust-lang/rust";

const NO_REBASE_WARN: &str = "Do NOT amend/squash/rebase any of the commits produced by this tool; that can badly break future syncs.";

pub enum RustcPullError {
    /// No changes are available to be pulled.
    NothingToPull,
    /// A rustc-pull has failed, probably a git operation error has occurred.
    PullFailed(anyhow::Error),
}

impl From<anyhow::Error> for RustcPullError {
    fn from(error: Error) -> Self {
        Self::PullFailed(error)
    }
}

#[derive(Copy, Clone)]
pub enum FilterVersion {
    /// Keep empty merge commits.
    Version1,
    /// Skip empty merge commits.
    Version2,
}

impl FilterVersion {
    pub fn latest() -> Self {
        Self::Version2
    }
}

#[derive(serde::Serialize, serde::Deserialize, Copy, Clone, Default)]
pub enum PullMode {
    /// Sync from the latest commit in the repo.
    #[default]
    Latest,
    /// Sync from the latest nightly commit.
    Nightly,
}

pub struct PullResult {
    pub merge_commit_message: String,
}

enum BumpedVersion {
    Latest {
        upstream_sha: String,
    },
    Nightly {
        upstream_sha: String,
        /// e.g. nightly-2026-09-18
        nightly: String,
    },
}

impl BumpedVersion {
    fn upstream_sha(&self) -> String {
        match self {
            BumpedVersion::Latest { upstream_sha } => upstream_sha.clone(),
            BumpedVersion::Nightly { upstream_sha, .. } => upstream_sha.clone(),
        }
    }
}

pub struct GitSync {
    context: SyncContext,
    proxy: JoshProxy,
    verbose: bool,
}

impl GitSync {
    pub fn new(context: SyncContext, proxy: JoshProxy, verbose: bool) -> Self {
        Self {
            context,
            proxy,
            verbose,
        }
    }

    pub fn rustc_pull(
        &self,
        upstream_repo: String,
        upstream_commit: Option<String>,
        allow_noop: bool,
    ) -> Result<PullResult, RustcPullError> {
        ensure_clean_git_state(self.verbose)?;

        let orig_head = get_current_head_sha(self.verbose)?;
        let previous_upstream_sha = self
            .context
            .last_upstream_sha
            .as_deref()
            .unwrap_or("<none>");
        println!("previous upstream base: {previous_upstream_sha}");
        println!("original local HEAD: {orig_head}");

        // Create a checkpoint to which we reset if something unusual happens
        let mut git_reset = GitResetOnDrop::new(orig_head, self.verbose);

        let bumped_version = self
            .bump_version_and_get_latest_upstream_sha(&upstream_repo, upstream_commit.as_ref())?;
        let upstream_sha = bumped_version.upstream_sha();

        println!("new upstream base: {upstream_sha}");

        let mut prep_message = format!(
            r#"Prepare for merging from {upstream_repo}

"#
        );
        match &bumped_version {
            BumpedVersion::Latest { upstream_sha } => write!(
                prep_message,
                "This updates the rust-version file to {upstream_sha}."
            )
            .unwrap(),
            BumpedVersion::Nightly {
                nightly,
                upstream_sha,
            } => {
                write!(
                    prep_message,
                    "This updates the rust-toolchain.toml file to {nightly} ({upstream_sha})."
                )
                .unwrap();
            }
        };

        let rust_version_path = self.context.rust_version_path.to_string_lossy();
        // Add the file to git index, in case this is the first time we perform the sync
        // Otherwise `git commit <file>` below wouldn't work.
        run_command(&["git", "add", &rust_version_path], self.verbose)?;
        run_command(
            &[
                "git",
                "commit",
                &rust_version_path,
                "--no-verify",
                "-m",
                &prep_message,
            ],
            self.verbose,
        )
        .context("cannot create preparation commit")?;

        // Make sure josh is running.
        let josh = self
            .proxy
            .start(&self.context.config)
            .context("cannot start josh-proxy")?;
        let josh_url = josh.git_url(
            &upstream_repo,
            Some(&upstream_sha),
            &construct_josh_filter(&self.context.config),
        );

        // Fetch given rustc commit.
        run_command(&["git", "fetch", &josh_url], self.verbose)
            .context("cannot fetch git state through Josh")?;

        // This should not add any new root commits. So count those before and after merging.
        let num_roots = || -> anyhow::Result<u32> {
            Ok(run_command(
                &["git", "rev-list", "HEAD", "--max-parents=0", "--count"],
                self.verbose,
            )
            .context("failed to determine the number of root commits")?
            .parse::<u32>()?)
        };
        let num_roots_before = num_roots()?;

        let sha_pre_merge = get_current_head_sha(self.verbose)?;

        // The filtered SHA of upstream
        let incoming_ref = run_command(["git", "rev-parse", "FETCH_HEAD"], self.verbose)?;
        println!("incoming ref: {incoming_ref}");

        let upstream_label = match &bumped_version {
            BumpedVersion::Latest { upstream_sha } => {
                format!("'{}'", &upstream_sha[..12])
            }
            BumpedVersion::Nightly {
                nightly,
                upstream_sha,
            } => {
                format!("'{}' ({nightly})", &upstream_sha[..12])
            }
        };
        let merge_message = format!(
            r#"Merge ref {upstream_label} from {upstream_repo}

Pull recent changes from https://github.com/{upstream_repo} via Josh.

Previous upstream ref: {upstream_repo}@{previous_upstream_sha}
New upstream ref: {upstream_repo}@{upstream_sha}
Filtered ref: {sub_org}/{sub_repo}@{incoming_ref}
Upstream diff: https://github.com/{DEFAULT_UPSTREAM_REPO}/compare/{prev_upstream_sha}...{upstream_sha}

This merge was created using https://github.com/rust-lang/josh-sync.
"#,
            sub_org = self.context.config.org,
            sub_repo = self.context.config.repo,
            prev_upstream_sha = self
                .context
                .last_upstream_sha
                .as_deref()
                .unwrap_or(&upstream_sha)
        );

        // Merge the fetched commit.
        // It is useful to print stdout/stderr here, because it shows the git diff summary
        if let Err(error) = stream_command(
            &[
                "git",
                "merge",
                "FETCH_HEAD",
                "--no-verify",
                "--no-ff",
                "-m",
                &merge_message,
            ],
            self.verbose,
        )
        .context("FAILED to merge new commits, something went wrong")
        {
            eprintln!(
                r"The merge was unsuccessful (maybe there was a conflict?).
NOT rolling back the branch state, so you can examine it manually.
After you fix the conflicts, `git add` the changes and run `git merge --continue`."
            );
            eprintln!("{NO_REBASE_WARN}");
            git_reset.disarm();
            return Err(RustcPullError::PullFailed(error));
        }

        // Now detect if something has actually been pulled
        let current_sha = get_current_head_sha(self.verbose)?;

        // This is the easy case, no merge was performed, so we bail, unless `allow_noop` is true
        if current_sha == sha_pre_merge && !allow_noop {
            eprintln!("No merge was performed, no changes to pull were found. Rolling back.");
            return Err(RustcPullError::NothingToPull);
        }

        // But it can be more tricky - we can have only empty merge/rollup merge commits from
        // rustc, so a merge was created, but the in-tree diff can still be empty.
        // In that case we also bail, unless `allow_noop` is true.
        if self.has_empty_diff(&sha_pre_merge) && !allow_noop {
            eprintln!("Only empty changes were pulled. Rolling back.");
            return Err(RustcPullError::NothingToPull);
        }

        println!("Pull finished! Current HEAD is {current_sha}");
        println!("{NO_REBASE_WARN}");

        if !self.context.config.post_pull.is_empty() {
            println!("Running post-pull operation(s)");

            for op in &self.context.config.post_pull {
                self.run_post_pull_op(&op)?;
            }
        }

        git_reset.disarm();

        // Check that the number of roots did not change.
        if num_roots()? != num_roots_before {
            return Err(anyhow::anyhow!(
                "Josh created a new root commit. This is probably not the history you want."
            )
            .into());
        }

        Ok(PullResult {
            merge_commit_message: merge_message,
        })
    }

    pub fn rustc_push(&self, username: &str, branch: &str) -> anyhow::Result<()> {
        ensure_clean_git_state(self.verbose)?;

        let base_upstream_sha = self.context.last_upstream_sha.clone().unwrap_or_default();

        // Make sure josh is running.
        let josh = self
            .proxy
            .start(&self.context.config)
            .context("cannot start josh-proxy")?;
        let josh_url = josh.git_url(
            &format!("{username}/rust"),
            None,
            &construct_josh_filter(&self.context.config),
        );
        let user_upstream_url = format!("https://github.com/{username}/rust");

        let rustc_git =
            prepare_rustc_checkout(self.verbose).context("cannot prepare rustc checkout")?;

        // Prepare the branch. Pushing works much better if we use as base exactly
        // the commit that we pulled from last time, so we use the `rust-version`
        // file to find out which commit that would be.
        println!("Preparing {user_upstream_url} (base: {base_upstream_sha})...");

        // Check if the remote branch doesn't already exist
        if run_command_at(
            &["git", "fetch", &user_upstream_url, branch],
            &rustc_git,
            self.verbose,
        )
        .is_ok()
        {
            return Err(anyhow::anyhow!(
                "The branch '{branch}' seems to already exist in '{user_upstream_url}'. Please delete it and try again."
            ));
        }

        // Download the base upstream SHA
        run_command_at(
            &[
                "git",
                "fetch",
                &format!("https://github.com/{DEFAULT_UPSTREAM_REPO}"),
                &base_upstream_sha,
            ],
            &rustc_git,
            self.verbose,
        )
        .context("cannot download latest upstream SHA")?;

        // And push it to the user's fork's branch
        run_command_at(
            &[
                "git",
                "push",
                &user_upstream_url,
                &format!("{base_upstream_sha}:refs/heads/{branch}"),
            ],
            &rustc_git,
            self.verbose,
        )
        .context("cannot push to your fork")?;
        println!();

        // Do the actual push from the subtree git repo
        println!("Pushing changes...");
        run_command(
            &["git", "push", &josh_url, &format!("HEAD:{branch}")],
            self.verbose,
        )?;
        println!();

        // Do a round-trip check to make sure the push worked as expected.
        self.roundtrip_check(&self.context.config, &josh_url, &branch)?;
        println!("{NO_REBASE_WARN}");

        Ok(())
    }

    fn has_empty_diff(&self, baseline_sha: &str) -> bool {
        // `git diff --exit-code` "succeeds" if the diff is empty.
        run_command(&["git", "diff", "--exit-code", baseline_sha], self.verbose).is_ok()
    }

    fn run_post_pull_op(&self, op: &PostPullOperation) -> anyhow::Result<()> {
        let head = get_current_head_sha(self.verbose)?;
        run_command(op.cmd.iter().map(|s| s.as_str()).collect::<Vec<_>>(), true)?;
        if !self.has_empty_diff(&head) {
            println!(
                "`{}` changed something, committing with message `{}`",
                op.cmd.join(" "),
                op.commit_message
            );
            run_command(["git", "add", "-u"], self.verbose)?;
            run_command(["git", "commit", "-m", &op.commit_message], self.verbose)?;
        }

        Ok(())
    }

    fn roundtrip_check(
        &self,
        config: &JoshConfig,
        josh_url: &str,
        branch: &str,
    ) -> anyhow::Result<()> {
        run_command_at(
            &["git", "fetch", josh_url, branch],
            &std::env::current_dir().unwrap(),
            self.verbose,
        )?;
        let head = if let Some(subtree_filter) = &config.subtree_filter {
            let josh_filter = get_josh_filter(self.verbose)?;
            josh_filter.run(
                &[subtree_filter, "HEAD"],
                &std::env::current_dir().unwrap(),
                self.verbose,
            )?;
            run_command(&["git", "rev-parse", "FILTERED_HEAD"], self.verbose)
                .context("failed to get FILTERED_HEAD")?
        } else {
            get_current_head_sha(self.verbose)?
        };
        let fetch_head = run_command(&["git", "rev-parse", "FETCH_HEAD"], self.verbose)?;
        if head != fetch_head {
            return Err(anyhow::anyhow!(
                "Josh created a non-roundtrip push! Do NOT merge this into rustc!\n\
                Expected {head}, got {fetch_head}."
            ));
        }
        println!(
            "Confirmed that the push round-trips back to {} properly. Please create a rustc PR.",
            self.context.config.repo
        );
        Ok(())
    }

    /// Returns the upstream_sha to be used for the pull, and update the version file to this
    /// version "in-place" on disk.
    fn bump_version_and_get_latest_upstream_sha(
        &self,
        upstream_repo: &str,
        upstream_commit: Option<&String>,
    ) -> Result<BumpedVersion, RustcPullError> {
        match self.context.config.pull_mode {
            PullMode::Latest => self.bump_version_latest(upstream_repo, upstream_commit),
            PullMode::Nightly => self.bump_version_nightly(upstream_commit),
        }
    }

    fn bump_version_latest(
        &self,
        upstream_repo: &str,
        upstream_commit: Option<&String>,
    ) -> Result<BumpedVersion, RustcPullError> {
        // The upstream commit that we want to pull
        let upstream_sha = if let Some(sha) = upstream_commit {
            sha.clone()
        } else {
            let out = run_command(
                [
                    "git",
                    "ls-remote",
                    &format!("https://github.com/{upstream_repo}"),
                    "HEAD",
                ],
                self.verbose,
            )
            .context("cannot fetch upstream commit")?;
            out.split_whitespace()
                .next()
                .unwrap_or_else(|| panic!("Could not obtain Rust repo HEAD from remote: '{out}'"))
                .to_owned()
        };

        // If the upstream SHA hasn't changed from the latest sync, there is nothing to pull
        // We distinguish this situation for tools that might not want to consider this to
        // be an error.
        if let Some(previous_base_commit) = self.context.last_upstream_sha.as_ref() {
            if *previous_base_commit == upstream_sha {
                return Err(RustcPullError::NothingToPull);
            }
        }

        // Update the last upstream SHA file. As a separate commit, since making it part of
        // the merge has confused the heck out of josh in the past.
        // We pass `--no-verify` to avoid running git hooks.
        // We do this before the merge so that if there are merge conflicts, we have
        // the right rust-version file while resolving them.
        std::fs::write(
            &self.context.rust_version_path,
            &format!("{upstream_sha}\n"),
        )
        .with_context(|| {
            anyhow::anyhow!(
                "cannot write upstream SHA to {}",
                self.context.rust_version_path.display()
            )
        })?;

        Ok(BumpedVersion::Latest { upstream_sha })
    }

    fn bump_version_nightly(
        &self,
        upstream_commit: Option<&String>,
    ) -> Result<BumpedVersion, RustcPullError> {
        const MANIFEST_URL: &str = "https://static.rust-lang.org/dist/channel-rust-nightly.toml";

        let mut toml = std::fs::read_to_string(&self.context.rust_version_path)
            .with_context(|| {
                anyhow::anyhow!(
                    "cannot read rust-toolchain.toml file from {}",
                    self.context.rust_version_path.display()
                )
            })?
            .parse::<DocumentMut>()
            .with_context(|| {
                anyhow::anyhow!(
                    "cannot parse rust-toolchain.toml file from {}",
                    self.context.rust_version_path.display()
                )
            })?;
        let channel = toml
            .get_mut("toolchain")
            .with_context(|| {
                anyhow::anyhow!(
                    "cannot find `toolchain` key in rust-toolchain.toml file from {}",
                    self.context.rust_version_path.display()
                )
            })?
            .get_mut("channel")
            .with_context(|| {
                anyhow::anyhow!(
                    "cannot find `channel` key in rust-toolchain.toml file from {}",
                    self.context.rust_version_path.display()
                )
            })?;

        let (nightly, upstream_sha) = match upstream_commit {
            Some(nightly_date) => {
                // Here we treat `upstream_commit` as a nightly date
                let upstream_sha = nightly_date_to_sha(&nightly_date)?;
                (format!("nightly-{nightly_date}"), upstream_sha)
            }
            None => {
                // Parse the nightly manifest file to get the latest nightly date and the corresponding
                // upstream SHA for rust
                let nightly_manifest = ureq::get(MANIFEST_URL)
                    .call()
                    .with_context(|| {
                        anyhow::anyhow!("cannot fetch nightly manifest from {MANIFEST_URL}")
                    })?
                    .body_mut()
                    .read_to_string()
                    .with_context(|| anyhow::anyhow!("cannot read nightly manifest"))?
                    .parse::<Document<_>>()
                    .with_context(|| anyhow::anyhow!("cannot parse nightly manifest as TOML"))?;
                let date = nightly_manifest
                    .get("date")
                    .and_then(|v| v.as_str())
                    .with_context(|| {
                        anyhow::anyhow!("cannot find `date` key in nightly manifest")
                    })?;
                let nightly = format!("nightly-{date}");
                let upstream_sha = nightly_manifest
                    .get("pkg")
                    .and_then(|v| v.get("rust"))
                    .and_then(|v| v.get("git_commit_hash"))
                    .and_then(|v| v.as_str())
                    .with_context(|| {
                        anyhow::anyhow!(
                            "cannot find `pkg.rust.git_commit_hash` key in nightly manifest"
                        )
                    })?
                    .to_string();
                (nightly, upstream_sha)
            }
        };

        // Override the nightly version in the TOML file and write it back
        *channel = toml_edit::value(&nightly);
        std::fs::write(&self.context.rust_version_path, toml.to_string()).with_context(|| {
            anyhow::anyhow!(
                "cannot write rust-toolchain.toml file to {}",
                self.context.rust_version_path.display()
            )
        })?;

        Ok(BumpedVersion::Nightly {
            upstream_sha,
            nightly,
        })
    }
}

// This is called only when the `subtree-filter` is set.
fn get_josh_filter(verbose: bool) -> anyhow::Result<JoshFilter> {
    println!("Updating/installing josh-filter binary...");
    match try_install_josh_filter(verbose) {
        Some(filter) => Ok(filter),
        None => Err(anyhow::anyhow!("Could not install josh-filter")),
    }
}

/// Find a rustc repo we can do our push preparation in.
fn prepare_rustc_checkout(verbose: bool) -> anyhow::Result<PathBuf> {
    if let Ok(rustc_git) = std::env::var("RUSTC_GIT") {
        let rustc_git = PathBuf::from(rustc_git);
        assert!(
            rustc_git.is_dir(),
            "rustc checkout path must be a directory"
        );
        return Ok(rustc_git);
    };

    // Otherwise, download it
    let path = "rustc-checkout";
    if !Path::new(path).join(".git").exists() {
        if prompt(
            &format!(
                "Path to a rustc checkout is not configured via the RUSTC_GIT environment variable, and {path} directory was not found. Do you want to download a rustc checkout into {path}?",
            ),
            // Download git history if we are on CI
            true,
        ) {
            println!(
                "Cloning rustc into `{path}`. Use RUSTC_GIT environment variable to override the location of the checkout"
            );
            // Stream stdout/stderr to the terminal, so that the user sees clone progress
            stream_command(
                &[
                    "git",
                    "clone",
                    "--filter=blob:none",
                    &format!("https://github.com/{DEFAULT_UPSTREAM_REPO}"),
                    path,
                ],
                verbose,
            )
            .context("cannot clone rustc")?;
        } else {
            return Err(anyhow::anyhow!("cannot continue without a rustc checkout"));
        }
    }
    Ok(PathBuf::from(path))
}

/// Restores HEAD to `reset_to` on drop, unless `disarm` is called first.
struct GitResetOnDrop {
    disarmed: bool,
    reset_to: String,
    verbose: bool,
}

impl GitResetOnDrop {
    fn new(current_sha: String, verbose: bool) -> Self {
        Self {
            disarmed: false,
            reset_to: current_sha,
            verbose,
        }
    }

    fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for GitResetOnDrop {
    fn drop(&mut self) {
        if !self.disarmed {
            eprintln!("Reverting HEAD to {}", self.reset_to);
            run_command(&["git", "reset", "--hard", &self.reset_to], self.verbose)
                .expect(&format!("cannot reset current branch to {}", self.reset_to));
        }
    }
}

fn construct_josh_filter(config: &JoshConfig) -> String {
    let filter = match (&config.path, &config.filter) {
        (Some(path), None) => format!(":/{path}"),
        (None, Some(filter)) => filter.clone(),
        _ => panic!("Config contains both path and a filter"),
    };
    match config.filter_version {
        // Keep backwards compatibility with repositories that started with a legacy version of
        // Josh.
        FilterVersion::Version1 => {
            // Convert old :rev syntax
            let filter = convert_rev_syntax(&filter);
            // Keep empty merges
            wrap_compat(&filter)
        }
        // Use the current default behavior of Josh.
        FilterVersion::Version2 => filter,
    }
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
