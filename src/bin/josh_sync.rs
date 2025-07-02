use anyhow::Context;
use clap::Parser;
use josh_sync::JoshConfig;
use josh_sync::josh::{JoshProxy, try_install_josh};
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
                upstream_sha: None,
            };
            let config = toml::to_string_pretty(&config).context("cannot serialize config")?;
            std::fs::write(DEFAULT_CONFIG_PATH, config).context("cannot write config")?;
            println!("Created config file at {DEFAULT_CONFIG_PATH}");
        }
        Command::Pull { config } => {
            let config = load_config(&config)
                .context("cannot load config. Run the `init` command to initialize it.")?;
            let josh = get_josh_proxy()?;
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

fn load_config(path: &Path) -> anyhow::Result<JoshConfig> {
    let data = std::fs::read_to_string(path)
        .with_context(|| format!("cannot load config file from {}", path.display()))?;
    let config: JoshConfig = toml::from_str(&data).context("cannot load config as TOML")?;
    Ok(config)
}
