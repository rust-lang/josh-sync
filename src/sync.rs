use crate::config::JoshConfigWithPath;
use crate::josh::JoshProxy;
use crate::utils::{check_output, ensure_clean_git_state};
use anyhow::{Context, Error};

const UPSTREAM_REPO: &str = "rust-lang/rust";

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

/// Constructs a Josh filter for synchronization, like `:/src/foo`.
fn construct_filter(path: &str) -> String {
    format!(":/{path}")
}

pub struct PullResult {
    pub merge_commit_message: String,
}

pub struct GitSync {
    config: JoshConfigWithPath,
    proxy: JoshProxy,
}

impl GitSync {
    pub fn new(config: JoshConfigWithPath, proxy: JoshProxy) -> Self {
        Self { config, proxy }
    }

    pub fn rustc_pull(&self) -> Result<PullResult, RustcPullError> {
        // The upstream commit that we want to pull
        let upstream_sha = {
            let out = check_output([
                "git",
                "ls-remote",
                &format!("https://github.com/{UPSTREAM_REPO}"),
                "HEAD",
            ])
            .context("cannot fetch upstream commit")?;
            out.split_whitespace()
                .next()
                .unwrap_or_else(|| panic!("Could not obtain Rust repo HEAD from remote: '{out}'"))
                .to_owned()
        };

        ensure_clean_git_state();

        // Make sure josh is running.
        let josh = self
            .proxy
            .start(&self.config.config)
            .context("cannot start josh-proxy")?;
        let josh_url = josh.git_url(
            UPSTREAM_REPO,
            &upstream_sha,
            &construct_filter(&self.config.config.path),
        );

        let orig_head = check_output(["git", "rev-parse", "HEAD"])?;
        println!(
            "previous upstream base: {:?}",
            self.config.config.last_upstream_sha
        );
        println!("current upstream base: {upstream_sha}");
        println!("original local HEAD: {orig_head}");

        if let Some(previous_base_commit) = self.config.config.last_upstream_sha.as_ref() {
            if *previous_base_commit == upstream_sha {
                return Err(RustcPullError::NothingToPull);
            }
        }

        // Update the last upstream SHA file. As a separate commit, since making it part of
        // the merge has confused the heck out of josh in the past.
        // We pass `--no-verify` to avoid running git hooks.
        // We do this before the merge so that if there are merge conflicts, we have
        // the right rust-version file while resolving them.
        let mut config = self.config.config.clone();
        config.last_upstream_sha = Some(upstream_sha.clone());
        config.write(&self.config.path)?;

        let prep_message = format!(
            r#"Update the upstream Rust SHA to {upstream_sha}
To prepare for merging from {UPSTREAM_REPO}"#,
        );

        let config_path = self.config.path.to_string_lossy().to_string();
        check_output(&["git", "add", &config_path])?;
        check_output(&[
            "git",
            "commit",
            &config_path,
            "--no-verify",
            "-m",
            &prep_message,
        ])
        .context("cannot create preparation commit")?;

        // Make sure that we reset the above commit if something fails
        let mut git_reset = GitResetOnDrop::default();

        // Fetch given rustc commit.
        check_output(&["git", "fetch", &josh_url])
            .context("cannot fetch git state through Josh")?;

        // This should not add any new root commits. So count those before and after merging.
        let num_roots = || -> anyhow::Result<u32> {
            Ok(
                check_output(&["git", "rev-list", "HEAD", "--max-parents=0", "--count"])
                    .context("failed to determine the number of root commits")?
                    .parse::<u32>()?,
            )
        };
        let num_roots_before = num_roots()?;

        let sha =
            check_output(&["git", "rev-parse", "HEAD"]).context("failed to get current commit")?;

        // The filtered SHA of upstream
        let incoming_ref = check_output(["git", "rev-parse", "FETCH_HEAD"])?;
        println!("incoming ref: {incoming_ref}");

        let merge_message = format!(
            r#"Merge ref '{upstream_head_short}{filter}' from {UPSTREAM_REPO}

Pull recent changes from {UPSTREAM_REPO} via Josh.

Upstream ref: {upstream_sha}
Filtered ref: {incoming_ref}
            "#,
            upstream_head_short = &upstream_sha[..12],
            filter = construct_filter(&self.config.config.path)
        );

        // Merge the fetched commit.
        check_output(&[
            "git",
            "merge",
            "FETCH_HEAD",
            "--no-verify",
            "--no-ff",
            "-m",
            &merge_message,
        ])
        .context("FAILED to merge new commits, something went wrong")?;

        let current_sha =
            check_output(&["git", "rev-parse", "HEAD"]).context("FAILED to get current commit")?;
        if current_sha == sha {
            eprintln!(
                "No merge was performed, no changes to pull were found. Rolling back the preparation commit."
            );
            return Err(RustcPullError::NothingToPull);
        }

        git_reset.disarm();

        // Check that the number of roots did not increase.
        if num_roots()? != num_roots_before {
            return Err(anyhow::anyhow!(
                "Josh created a new root commit. This is probably not the history you want."
            )
            .into());
        }

        println!("Pull finished! Current HEAD is {current_sha}");
        Ok(PullResult {
            merge_commit_message: merge_message,
        })
    }
}

/// Removes the last commit on drop, unless `disarm` is called first.
#[derive(Default)]
struct GitResetOnDrop {
    disarmed: bool,
}

impl GitResetOnDrop {
    fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for GitResetOnDrop {
    fn drop(&mut self) {
        if !self.disarmed {
            eprintln!("Reverting last commit");
            check_output(&["git", "reset", "--hard", "HEAD^"])
                .expect("cannot reset last git commit");
        }
    }
}
