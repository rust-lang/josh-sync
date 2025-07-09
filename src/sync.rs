use crate::SyncContext;
use crate::josh::JoshProxy;
use crate::utils::{ensure_clean_git_state, prompt};
use crate::utils::{get_current_head_sha, run_command_at};
use crate::utils::{run_command, stream_command};
use anyhow::{Context, Error};
use std::path::{Path, PathBuf};

pub const UPSTREAM_REPO: &str = "rust-lang/rust";

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

pub struct PullResult {
    pub merge_commit_message: String,
}

pub struct GitSync {
    context: SyncContext,
    proxy: JoshProxy,
}

impl GitSync {
    pub fn new(context: SyncContext, proxy: JoshProxy) -> Self {
        Self { context, proxy }
    }

    pub fn rustc_pull(&self) -> Result<PullResult, RustcPullError> {
        // The upstream commit that we want to pull
        let upstream_sha = {
            let out = run_command([
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
            .start(&self.context.config)
            .context("cannot start josh-proxy")?;
        let josh_url = josh.git_url(
            UPSTREAM_REPO,
            Some(&upstream_sha),
            &self.context.config.construct_josh_filter(),
        );

        let orig_head = get_current_head_sha()?;
        println!(
            "previous upstream base: {:?}",
            self.context.last_upstream_sha
        );
        println!("new upstream base: {upstream_sha}");
        println!("original local HEAD: {orig_head}");

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
            &self.context.last_upstream_sha_path,
            &format!("{upstream_sha}\n"),
        )
        .with_context(|| {
            anyhow::anyhow!(
                "cannot write upstream SHA to {}",
                self.context.last_upstream_sha_path.display()
            )
        })?;

        let prep_message = format!(
            r#"Prepare for merging from {UPSTREAM_REPO}

This updates the rust-version file to {upstream_sha}."#,
        );

        let rust_version_path = self
            .context
            .last_upstream_sha_path
            .to_string_lossy()
            .to_string();
        // Add the file to git index, in case this is the first time we perform the sync
        // Otherwise `git commit <file>` below wouldn't work.
        run_command(&["git", "add", &rust_version_path])?;
        run_command(&[
            "git",
            "commit",
            &rust_version_path,
            "--no-verify",
            "-m",
            &prep_message,
        ])
        .context("cannot create preparation commit")?;

        // Make sure that we reset the above commit if something fails
        let mut git_reset = GitResetOnDrop::new(orig_head);

        // Fetch given rustc commit.
        run_command(&["git", "fetch", &josh_url]).context("cannot fetch git state through Josh")?;

        // This should not add any new root commits. So count those before and after merging.
        let num_roots = || -> anyhow::Result<u32> {
            Ok(
                run_command(&["git", "rev-list", "HEAD", "--max-parents=0", "--count"])
                    .context("failed to determine the number of root commits")?
                    .parse::<u32>()?,
            )
        };
        let num_roots_before = num_roots()?;

        let sha_pre_merge = get_current_head_sha()?;

        // The filtered SHA of upstream
        let incoming_ref = run_command(["git", "rev-parse", "FETCH_HEAD"])?;
        println!("incoming ref: {incoming_ref}");

        let merge_message = format!(
            r#"Merge ref '{upstream_head_short}' from {UPSTREAM_REPO}

Pull recent changes from https://github.com/{UPSTREAM_REPO} via Josh.

Upstream ref: {upstream_sha}
Filtered ref: {incoming_ref}

This merge was created using https://github.com/rust-lang/josh-sync.
"#,
            upstream_head_short = &upstream_sha[..12],
        );

        // Merge the fetched commit.
        // It is useful to print stdout/stderr here, because it shows the git diff summary
        stream_command(&[
            "git",
            "merge",
            "FETCH_HEAD",
            "--no-verify",
            "--no-ff",
            "-m",
            &merge_message,
        ])
        .context("FAILED to merge new commits, something went wrong")?;

        // Now detect if something has actually been pulled
        let current_sha = get_current_head_sha()?;

        // This is the easy case, no merge was performed, so we bail
        if current_sha == sha_pre_merge {
            eprintln!(
                "No merge was performed, no changes to pull were found. Rolling back the preparation commit."
            );
            return Err(RustcPullError::NothingToPull);
        }

        // But it can be more tricky - we can have only empty merge/rollup merge commits from
        // rustc, so a merge was created, but the in-tree diff can still be empty.
        // In that case we also bail.
        // `git diff --exit-code` "succeeds" if the diff is empty.
        if run_command(&["git", "diff", "--exit-code", &sha_pre_merge]).is_ok() {
            eprintln!("Only empty changes were pulled. Rolling back the preparation commit.");
            return Err(RustcPullError::NothingToPull);
        }

        git_reset.disarm();

        // Check that the number of roots did not change.
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

    pub fn rustc_push(&self, username: &str, branch: &str) -> anyhow::Result<()> {
        ensure_clean_git_state();

        let base_upstream_sha = self.context.last_upstream_sha.clone().unwrap_or_default();

        // Make sure josh is running.
        let josh = self
            .proxy
            .start(&self.context.config)
            .context("cannot start josh-proxy")?;
        let josh_url = josh.git_url(
            &format!("{username}/rust"),
            None,
            &self.context.config.construct_josh_filter(),
        );
        let user_upstream_url = format!("https://github.com/{username}/rust");

        let rustc_git = prepare_rustc_checkout().context("cannot prepare rustc checkout")?;

        // Prepare the branch. Pushing works much better if we use as base exactly
        // the commit that we pulled from last time, so we use the `rust-version`
        // file to find out which commit that would be.
        println!("Preparing {user_upstream_url} (base: {base_upstream_sha})...");

        // Check if the remote branch doesn't already exist
        if run_command_at(&["git", "fetch", &user_upstream_url, branch], &rustc_git).is_ok() {
            return Err(anyhow::anyhow!(
                "The branch '{branch}' seems to already exist in '{user_upstream_url}'. Please delete it and try again."
            ));
        }

        // Download the base upstream SHA
        run_command_at(
            &[
                "git",
                "fetch",
                &format!("https://github.com/{UPSTREAM_REPO}"),
                &base_upstream_sha,
            ],
            &rustc_git,
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
        )
        .context("cannot push to your fork")?;
        println!();

        // Do the actual push from the subtree git repo
        println!("Pushing changes...");
        run_command(&["git", "push", &josh_url, &format!("HEAD:{branch}")])?;
        println!();

        // Do a round-trip check to make sure the push worked as expected.
        run_command_at(
            &["git", "fetch", &josh_url, &branch],
            &std::env::current_dir().unwrap(),
        )?;
        let head = get_current_head_sha()?;
        let fetch_head = run_command(&["git", "rev-parse", "FETCH_HEAD"])?;
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
}

/// Find a rustc repo we can do our push preparation in.
fn prepare_rustc_checkout() -> anyhow::Result<PathBuf> {
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
            stream_command(&[
                "git",
                "clone",
                "--filter=blob:none",
                "https://github.com/rust-lang/rust",
                path,
            ])
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
}

impl GitResetOnDrop {
    fn new(current_sha: String) -> Self {
        Self {
            disarmed: false,
            reset_to: current_sha,
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
            run_command(&["git", "reset", "--hard", &self.reset_to])
                .expect(&format!("cannot reset current branch to {}", self.reset_to));
        }
    }
}
