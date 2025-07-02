use anyhow::Context;
use clap::Parser;
use josh_sync::config::{JoshConfig, load_config};
use josh_sync::josh::{JoshProxy, try_install_josh};
use josh_sync::sync::{GitSync, RustcPullError, UPSTREAM_REPO};
use josh_sync::utils::prompt;
use std::path::{Path, PathBuf};

const DEFAULT_CONFIG_PATH: &str = "josh-sync.toml";

#[derive(clap::Parser)]
struct Args {
    #[clap(subcommand)]
    cmd: Command,
}

#[derive(clap::Parser)]
enum Command {
    /// Initialize a config file for this repository.
    Init,
    /// Pull changes from the main `rust-lang/rust` repository.
    /// This creates new commits that should be then merged into this subtree repository.
    Pull {
        #[clap(long, default_value(DEFAULT_CONFIG_PATH))]
        config: PathBuf,
    },
    /// Push changes into the main `rust-lang/rust` repository `branch` of a `rustc` fork under
    /// the given GitHub `username`.
    /// The pushed branch should then be merged into the `rustc` repository.
    Push {
        #[clap(long, default_value(DEFAULT_CONFIG_PATH))]
        config: PathBuf,
        /// Branch that should be pushed to your remote
        branch: String,
        /// Your GitHub usename where the fork is located
        username: String,
    },
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    match args.cmd {
        Command::Init => {
            let config = JoshConfig {
                org: "rust-lang".to_string(),
                repo: "<repository-name>".to_string(),
                path: "<relative-subtree-path>".to_string(),
                last_upstream_sha: None,
            };
            config
                .write(Path::new(DEFAULT_CONFIG_PATH))
                .context("cannot write config")?;
            println!("Created config file at {DEFAULT_CONFIG_PATH}");
        }
        Command::Pull { config } => {
            let config = load_config(&config)
                .context("cannot load config. Run the `init` command to initialize it.")?;
            let josh = get_josh_proxy()?;
            let sync = GitSync::new(config.clone(), josh);
            match sync.rustc_pull() {
                Ok(result) => {
                    maybe_create_gh_pr(
                        &config.config.full_repo_name(),
                        "Rustc pull update",
                        &result.merge_commit_message,
                    )?;
                }
                Err(RustcPullError::NothingToPull) => {
                    eprintln!("Nothing to pull");
                    std::process::exit(2);
                }
                Err(RustcPullError::PullFailed(error)) => {
                    eprintln!("Pull failure: {error:?}");
                    std::process::exit(1);
                }
            }
        }
        Command::Push {
            username,
            branch,
            config,
        } => {
            let config = load_config(&config)
                .context("cannot load config. Run the `init` command to initialize it.")?;
            let josh = get_josh_proxy()?;
            let sync = GitSync::new(config.clone(), josh);
            sync.rustc_push(&username, &branch)
                .context("cannot perform push")?;

            // Open PR with `subtree update` title to silence the `no-merges` triagebot check
            println!(
                r#"You can create the rustc PR using the following URL:
https://github.com/{UPSTREAM_REPO}/compare/{username}:{branch}?quick_pull=1&title={}+subtree+update&body=r?+@ghost"#,
                config.config.repo
            );
        }
    }

    Ok(())
}

fn maybe_create_gh_pr(repo: &str, title: &str, description: &str) -> anyhow::Result<bool> {
    let gh_available = which::which("gh").is_ok();
    if !gh_available {
        println!(
            "Note: if you install the `gh` CLI tool, josh-sync will be able to create the sync PR for you."
        );
        Ok(false)
    } else if prompt("Do you want to create a rustc pull PR using the `gh` tool?") {
        std::process::Command::new("gh")
            .args(&[
                "pr",
                "create",
                "--title",
                title,
                "--body",
                description,
                "--repo",
                repo,
            ])
            .spawn()?
            .wait()?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn get_josh_proxy() -> anyhow::Result<JoshProxy> {
    match JoshProxy::lookup() {
        Some(proxy) => Ok(proxy),
        None => {
            if prompt("josh-proxy not found. Do you want to install it?") {
                match try_install_josh() {
                    Some(proxy) => Ok(proxy),
                    None => Err(anyhow::anyhow!("Could not install josh-proxy")),
                }
            } else {
                Err(anyhow::anyhow!("josh-proxy could not be found"))
            }
        }
    }
}
