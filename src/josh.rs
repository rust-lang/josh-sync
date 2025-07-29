use crate::config::JoshConfig;
use crate::utils::run_command;
use anyhow::Context;
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

const JOSH_PORT: u16 = 42042;
/// Version of `josh-proxy` that should be downloaded for the user.
const JOSH_VERSION: &str = "r24.10.04";

pub struct JoshProxy {
    path: PathBuf,
}

impl JoshProxy {
    /// Tries to figure out if `josh-proxy` is installed.
    pub fn lookup() -> Option<Self> {
        which::which("josh-proxy").ok().map(|path| Self { path })
    }

    pub fn start(&self, config: &JoshConfig) -> anyhow::Result<RunningJoshProxy> {
        // Determine cache directory.
        let user_dirs =
            directories::ProjectDirs::from("org", &config.full_repo_name(), "rustc-josh")
                .context("cannot determine cache directory for Josh")?;
        let local_dir = user_dirs.cache_dir().to_owned();

        // Start josh, silencing its output.
        let josh = std::process::Command::new(&self.path)
            .arg("--local")
            .arg(local_dir)
            .args([
                "--remote=https://github.com",
                &format!("--port={JOSH_PORT}"),
                "--no-background",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to start josh-proxy, make sure it is installed")?;

        // Wait until the port is open. We try every 10ms until 1s passed.
        for _ in 0..100 {
            // This will generally fail immediately when the port is still closed.
            let addr = SocketAddr::from(([127, 0, 0, 1], JOSH_PORT));
            let josh_ready = TcpStream::connect_timeout(&addr, Duration::from_millis(1));

            if josh_ready.is_ok() {
                println!("josh up and running");
                return Ok(RunningJoshProxy {
                    process: josh,
                    port: JOSH_PORT,
                });
            }

            // Not ready yet.
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("Even after waiting for 1s, josh-proxy is still not available.");
    }
}

/// Try to install (or update) josh-proxy, to make sure that we use the correct version.
pub fn try_install_josh(verbose: bool) -> Option<JoshProxy> {
    run_command(
        &[
            "cargo",
            "install",
            "--locked",
            "--git",
            "https://github.com/josh-project/josh",
            "--tag",
            JOSH_VERSION,
            "josh-proxy",
        ],
        verbose,
    )
    .expect("cannot install josh-proxy");
    JoshProxy::lookup()
}

/// Create a wrapper that represents a running instance of `josh-proxy` and stops it on drop.
pub struct RunningJoshProxy {
    process: std::process::Child,
    port: u16,
}

impl RunningJoshProxy {
    pub fn git_url(&self, repo: &str, commit: Option<&str>, filter: &str) -> String {
        let commit = commit.map(|c| format!("@{c}")).unwrap_or_default();
        format!(
            "http://localhost:{}/{repo}.git{commit}{filter}.git",
            self.port
        )
    }
}

impl Drop for RunningJoshProxy {
    fn drop(&mut self) {
        if cfg!(unix) {
            // Try to gracefully shut it down.
            Command::new("kill")
                .args(["-s", "INT", &self.process.id().to_string()])
                .output()
                .expect("failed to SIGINT josh-proxy");
            // Sadly there is no "wait with timeout"... so we just give it some time to finish.
            std::thread::sleep(Duration::from_millis(100));
            // Now hopefully it is gone.
            if self
                .process
                .try_wait()
                .expect("failed to wait for josh-proxy")
                .is_some()
            {
                return;
            }
        }
        // If that didn't work (or we're not on Unix), kill it hard.
        eprintln!(
            "I have to kill josh-proxy the hard way, let's hope this does not \
            break anything."
        );
        self.process.kill().expect("failed to SIGKILL josh-proxy");
    }
}
