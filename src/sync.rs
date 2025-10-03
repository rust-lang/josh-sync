use crate::SyncContext;
use crate::config::PostPullOperation;
use crate::josh::JoshProxy;
use crate::utils::{ensure_clean_git_state, prompt};
use crate::utils::{get_current_head_sha, run_command_at};
use crate::utils::{run_command, stream_command};
use anyhow::{Context, Error};
use std::path::{Path, PathBuf};

pub const DEFAULT_UPSTREAM_REPO: &str = "rust-lang/rust";

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
        // The upstream commit that we want to pull
        let upstream_sha = if let Some(sha) = upstream_commit {
            sha
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

        ensure_clean_git_state(self.verbose)?;

        // Make sure josh is running.
        let josh = self
            .proxy
            .start(&self.context.config)
            .context("cannot start josh-proxy")?;
        let josh_url = josh.git_url(
            &upstream_repo,
            Some(&upstream_sha),
            &self.context.config.construct_josh_filter(),
        );

        let orig_head = get_current_head_sha(self.verbose)?;
        println!(
            "previous upstream base: {}",
            self.context
                .last_upstream_sha
                .as_deref()
                .unwrap_or("<none>"),
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

        // Create a checkpoint to which we reset if something unusual happens
        let mut git_reset = GitResetOnDrop::new(orig_head, self.verbose);

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
            r#"Prepare for merging from {upstream_repo}

This updates the rust-version file to {upstream_sha}."#,
        );

        let rust_version_path = self
            .context
            .last_upstream_sha_path
            .to_string_lossy()
            .to_string();
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

        let merge_message = format!(
            r#"Merge ref '{upstream_head_short}' from {upstream_repo}

Pull recent changes from https://github.com/{upstream_repo} via Josh.

Upstream ref: {upstream_sha}
Filtered ref: {incoming_ref}
Upstream diff: https://github.com/{DEFAULT_UPSTREAM_REPO}/compare/{prev_upstream_sha}...{upstream_sha}

This merge was created using https://github.com/rust-lang/josh-sync.
"#,
            upstream_head_short = &upstream_sha[..12],
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
            &self.context.config.construct_josh_filter(),
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
        run_command_at(
            &["git", "fetch", &josh_url, &branch],
            &std::env::current_dir().unwrap(),
            self.verbose,
        )?;
        let head = get_current_head_sha(self.verbose)?;
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
