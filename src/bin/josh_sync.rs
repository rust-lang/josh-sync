use anyhow::Context;
use clap::Parser;
use josh_sync::config::{JoshConfig, load_config};
use josh_sync::josh::{JoshProxy, try_install_josh};
use josh_sync::sync::{GitSync, RustcPullError};
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
    /// Pull changes from the main `rustc` repository.
    /// This creates new commits that should be then merged into this subtree repository.
    Pull {
        #[clap(long, default_value(DEFAULT_CONFIG_PATH))]
        config: PathBuf,
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
            let sync = GitSync::new(config, josh);
            if let Err(error) = sync.rustc_pull() {
                match error {
                    RustcPullError::NothingToPull => {
                        eprintln!("Nothing to pull");
                        std::process::exit(2);
                    }
                    RustcPullError::PullFailed(error) => {
                        eprintln!("Pull failure: {error:?}");
                        std::process::exit(1);
                    }
                }
            }
        }
    }

    Ok(())
}

fn get_josh_proxy() -> anyhow::Result<JoshProxy> {
    match JoshProxy::lookup() {
        Some(proxy) => Ok(proxy),
        None => {
            println!("josh-proxy not found. Do you want to install it? [y/n]");
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            if line.trim().to_lowercase() == "y" {
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
