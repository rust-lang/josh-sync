use anyhow::Context;
use clap::Parser;
use rustc_josh_sync::JoshConfig;

const DEFAULT_CONFIG_PATH: &str = "josh-sync.toml";

#[derive(clap::Parser)]
struct Args {
    #[clap(subcommand)]
    cmd: Command
}

#[derive(clap::Parser)]
enum Command {
    /// Initialize a config file for this repository.
    Init
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
        }
    }

    Ok(())
}
