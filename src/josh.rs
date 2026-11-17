use crate::config::JoshConfig;
use crate::utils::{is_inside_ci, is_null_sha, run_command_by_path};
use anyhow::Context;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const JOSH_PORT: u16 = 42042;
const JOSH_STARTUP_TIMEOUT: Duration = Duration::from_secs(2);
/// Version of `josh-proxy` that should be downloaded for the user.
const JOSH_VERSION: &str = "r26.08.28";

pub struct JoshProxy {
    path: PathBuf,
}

impl JoshProxy {
    pub fn from_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn start(&self, config: &JoshConfig) -> anyhow::Result<RunningJoshProxy> {
        self.start_with_timeout(config, JOSH_PORT, JOSH_STARTUP_TIMEOUT)
    }

    fn start_with_timeout(
        &self,
        config: &JoshConfig,
        port: u16,
        timeout: Duration,
    ) -> anyhow::Result<RunningJoshProxy> {
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
                &format!("--port={port}"),
                "--no-background",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to start josh-proxy, make sure it is installed")?;
        let josh_guard = RunningJoshProxy {
            process: josh,
            port,
        };

        wait_for_proxy(port, timeout)?;
        println!("josh up and running");
        Ok(josh_guard)
    }
}

fn wait_for_proxy(port: u16, timeout: Duration) -> anyhow::Result<()> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        // This will generally fail immediately when the port is still closed.
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        if TcpStream::connect_timeout(&addr, Duration::from_millis(1)).is_ok() {
            return Ok(());
        }

        // Not ready yet.
        std::thread::sleep(Duration::from_millis(10).min(timeout.saturating_sub(start.elapsed())));
    }
    Err(anyhow::anyhow!(
        "josh-proxy did not become available on port {port} within {timeout:?}"
    ))
}

pub struct JoshFilter {
    path: PathBuf,
}

impl JoshFilter {
    pub fn from_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn run<'a, Args: AsRef<[&'a str]>>(
        &self,
        args: Args,
        workdir: &Path,
        verbose: bool,
    ) -> anyhow::Result<()> {
        let args = args.as_ref();
        let output = run_command_by_path(&self.path, args, workdir, true, verbose)?;
        if is_null_sha(&output) {
            return Err(anyhow::anyhow!(
                "josh-filter returned null SHA, filter may not match any content"
            ));
        }
        Ok(())
    }
}

fn josh_install_directory() -> PathBuf {
    // Install binaries globally (into CARGO_HOME) on CI to ensure better (rust-)cache usage
    if is_inside_ci()
        && let Some(dirs) = directories::UserDirs::new()
    {
        return dirs.home_dir().join(".cargo");
    }

    let Some(user_dirs) = directories::ProjectDirs::from("org", "rust-lang", "rustc-josh") else {
        eprintln!(
            "Cannot determine user directory for Josh installation, falling back to local directory"
        );
        return PathBuf::from(".josh-sync").join("cargo");
    };
    let local_dir = user_dirs.data_local_dir();
    local_dir.join("josh-sync").join("cargo")
}

#[derive(Copy, Clone, Debug)]
enum JoshProgram {
    Proxy,
    Filter,
}

pub fn try_install_josh_proxy(verbose: bool) -> Option<JoshProxy> {
    try_install_josh_program(JoshProgram::Proxy, verbose).map(JoshProxy::from_path)
}

pub fn try_install_josh_filter(verbose: bool) -> Option<JoshFilter> {
    try_install_josh_program(JoshProgram::Filter, verbose).map(JoshFilter::from_path)
}

/// Try to install (or update) a josh CLI program in a local installation directory.
/// Ensures that we use the correct version.
fn try_install_josh_program(program: JoshProgram, verbose: bool) -> Option<PathBuf> {
    let install_dir = josh_install_directory();
    let (krate, binary) = match program {
        JoshProgram::Proxy => ("josh-proxy", "josh-proxy"),
        JoshProgram::Filter => ("josh-cli", "josh-filter"),
    };
    let path = install_dir.join("bin").join(binary);
    println!(
        "Updating/installing {binary} binary into `{}`...",
        path.display()
    );

    let mut args = vec![
        "+stable",
        "install",
        "--locked",
        "--git",
        "https://github.com/josh-project/josh",
        "--tag",
        JOSH_VERSION,
        "--root",
        install_dir.to_str()?,
    ];

    args.push(krate);

    run_command_by_path(
        &Path::new("cargo"),
        &args,
        &std::env::current_dir().unwrap(),
        false,
        verbose,
    )
    .unwrap_or_else(|e| panic!("cannot install {binary}: {e:?}"));
    if path.is_file() { Some(path) } else { None }
}

/// Create a wrapper that represents a running instance of `josh-proxy` and stops it on drop.
pub struct RunningJoshProxy {
    process: std::process::Child,
    port: u16,
}

impl RunningJoshProxy {
    pub fn git_url(&self, repo: &str, commit: Option<&str>, filter: &str) -> String {
        let commit = commit.map(|c| format!("@{c}")).unwrap_or_default();
        let filter = urlencoding::encode(filter);
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
            // We try every 10ms until 1s passed.
            for _ in 0..100 {
                std::thread::sleep(Duration::from_millis(10));
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
        }
        // If that didn't work (or we're not on Unix), kill it hard.
        eprintln!(
            "I have to kill josh-proxy the hard way, let's hope this does not \
            break anything."
        );
        self.process.kill().expect("failed to SIGKILL josh-proxy");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn startup_timeout_returns_error() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let error = wait_for_proxy(port, Duration::from_millis(20)).unwrap_err();
        assert!(error.to_string().contains("did not become available"));
    }

    #[test]
    fn dropping_running_proxy_stops_child_process() {
        let process = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = process.id();
        assert!(process_is_running(pid));

        drop(RunningJoshProxy { process, port: 0 });

        assert!(!process_is_running(pid));
    }

    fn process_is_running(pid: u32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .unwrap()
            .status
            .success()
    }
}
