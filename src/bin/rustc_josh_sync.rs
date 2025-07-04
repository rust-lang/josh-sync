use anyhow::Context;
use clap::Parser;
use rustc_josh_sync::SyncContext;
use rustc_josh_sync::config::{JoshConfig, load_config};
use rustc_josh_sync::josh::{JoshProxy, try_install_josh};
use rustc_josh_sync::sync::{GitSync, RustcPullError, UPSTREAM_REPO};
use rustc_josh_sync::utils::prompt;
use std::path::{Path, PathBuf};

const DEFAULT_CONFIG_PATH: &str = "josh-sync.toml";
const DEFAULT_RUST_VERSION_PATH: &str = "rust-version";

#[derive(clap::Parser)]
struct Args {
    #[clap(subcommand)]
    cmd: Command,
}

#[derive(clap::Parser)]
enum Command {
    /// Initialize a config file and an empty `rust-version` file for this repository.
    Init,
    /// Pull changes from the main `rust-lang/rust` repository.
    /// This creates new commits that should be then merged into this subtree repository.
    Pull {
        #[clap(long, default_value(DEFAULT_CONFIG_PATH))]
        config_path: PathBuf,
        #[clap(long, default_value(DEFAULT_RUST_VERSION_PATH))]
        rust_version_path: PathBuf,
    },
    /// Push changes into the main `rust-lang/rust` repository `branch` of a `rustc` fork under
    /// the given GitHub `username`.
    /// The pushed branch should then be merged into the `rustc` repository.
    Push {
        #[clap(long, default_value(DEFAULT_CONFIG_PATH))]
        config_path: PathBuf,
        #[clap(long, default_value(DEFAULT_RUST_VERSION_PATH))]
        rust_version_path: PathBuf,
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
                path: Some("<relative-subtree-path>".to_string()),
                filter: None,
            };
            config
                .write(Path::new(DEFAULT_CONFIG_PATH))
                .context("cannot write config")?;
            println!("Created config file at {DEFAULT_CONFIG_PATH}");

            if !Path::new(DEFAULT_RUST_VERSION_PATH).is_file() {
                std::fs::write(DEFAULT_RUST_VERSION_PATH, "")
                    .context("cannot write rust-version file")?;
                println!("Created empty rust-version file at {DEFAULT_RUST_VERSION_PATH}");
            } else {
                println!("{DEFAULT_RUST_VERSION_PATH} already exists, not doing anything with it");
            }
        }
        Command::Pull {
            config_path,
            rust_version_path,
        } => {
            let ctx = load_context(&config_path, &rust_version_path)?;
            let josh = get_josh_proxy()?;
            let sync = GitSync::new(ctx.clone(), josh);
            match sync.rustc_pull() {
                Ok(result) => {
                    if !maybe_create_gh_pr(
                        &ctx.config.full_repo_name(),
                        "Rustc pull update",
                        &result.merge_commit_message,
                    )? {
                        println!(
                            "Now push the current branch to {} (either a fork or the main repo) and create a PR",
                            ctx.config.repo
                        );
                    }
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
            config_path,
            rust_version_path,
        } => {
            let ctx = load_context(&config_path, &rust_version_path)?;
            let josh = get_josh_proxy()?;
            let sync = GitSync::new(ctx.clone(), josh);
            sync.rustc_push(&username, &branch)
                .context("cannot perform push")?;

            // Open PR with `subtree update` title to silence the `no-merges` triagebot check
            let merge_msg = format!(
                r#"Subtree update of https://github.com/{}.

Created using https://github.com/rust-lang/josh-sync.

r? @ghost"#,
                ctx.config.full_repo_name(),
            );
            println!(
                r#"You can create the rustc PR using the following URL:
https://github.com/{UPSTREAM_REPO}/compare/{username}:{branch}?quick_pull=1&title={}+subtree+update&body={}"#,
                ctx.config.repo,
                urlencoding::encode(&merge_msg)
            );
        }
    }

    Ok(())
}

fn load_context(config_path: &Path, rust_version_path: &Path) -> anyhow::Result<SyncContext> {
    let config = load_config(&config_path)
        .context("cannot load config. Run the `init` command to initialize it.")?;
    let rust_version = std::fs::read_to_string(&rust_version_path)
        .inspect_err(|err| eprintln!("Cannot load rust-version file: {err:?}"))
        .map(|version| version.trim().to_string())
        .map(Some)
        .unwrap_or_default();
    Ok(SyncContext {
        config,
        last_upstream_sha_path: rust_version_path.to_path_buf(),
        last_upstream_sha: rust_version,
    })
}

fn maybe_create_gh_pr(repo: &str, title: &str, description: &str) -> anyhow::Result<bool> {
    if which::which("gh").is_ok()
        && prompt(
            "Do you want to create a rustc pull PR using the `gh` tool?",
            false,
        )
    {
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
    println!("Updating/installing josh-proxy binary...");
    match try_install_josh() {
        Some(proxy) => Ok(proxy),
        None => Err(anyhow::anyhow!("Could not install josh-proxy")),
    }
}
