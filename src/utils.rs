use anyhow::Context;
use std::path::Path;
use std::process::Command;

/// Run command and return its stdout.
pub fn run_command<'a, Args: AsRef<[&'a str]>>(
    args: Args,
    verbose: bool,
) -> anyhow::Result<String> {
    run_command_at(args, &std::env::current_dir()?, verbose)
}

/// Run command while streaming stdout and stderr to the terminal.
pub fn stream_command<'a, Args: AsRef<[&'a str]>>(args: Args, verbose: bool) -> anyhow::Result<()> {
    run_command_inner(args, &std::env::current_dir()?, false, verbose)?;
    Ok(())
}

pub fn run_command_at<'a, Args: AsRef<[&'a str]>>(
    args: Args,
    workdir: &Path,
    verbose: bool,
) -> anyhow::Result<String> {
    run_command_inner(args, workdir, true, verbose)
}

fn run_command_inner<'a, Args: AsRef<[&'a str]>>(
    args: Args,
    workdir: &Path,
    capture: bool,
    verbose: bool,
) -> anyhow::Result<String> {
    let args = args.as_ref();

    let mut cmd = Command::new(args[0]);
    cmd.current_dir(workdir);
    cmd.args(&args[1..]);

    if verbose {
        eprintln!("+ {cmd:?}");
    }
    if capture {
        let out = cmd.output().expect("command failed");
        let stdout = String::from_utf8_lossy(out.stdout.trim_ascii()).to_string();
        let stderr = String::from_utf8_lossy(out.stderr.trim_ascii()).to_string();
        if !out.status.success() {
            Err(anyhow::anyhow!(
                "Command `{cmd:?}` failed with exit code {:?}. STDOUT:\n{stdout}\nSTDERR:\n{stderr}",
                out.status.code()
            ))
        } else {
            Ok(stdout)
        }
    } else {
        let status = cmd
            .spawn()
            .expect("cannot spawn command")
            .wait()
            .expect("command failed");
        if !status.success() {
            Err(anyhow::anyhow!(
                "Command `{cmd:?}` failed with exit code {:?}",
                status.code()
            ))
        } else {
            Ok(String::new())
        }
    }
}

/// Fail if there are files that need to be checked in.
pub fn ensure_clean_git_state(verbose: bool) -> anyhow::Result<()> {
    let read = run_command(
        ["git", "status", "--untracked-files=no", "--porcelain"],
        verbose,
    )
    .expect("cannot figure out if git state is clean");
    if !read.is_empty() {
        Err(anyhow::anyhow!("working directory must be clean"))
    } else {
        Ok(())
    }
}

pub fn get_current_head_sha(verbose: bool) -> anyhow::Result<String> {
    run_command(&["git", "rev-parse", "HEAD"], verbose).context("failed to get current commit")
}

/// Ask a prompt to user and return true if they responded with `y`.
/// Returns `default_response` on CI.
pub fn prompt(prompt: &str, default_response: bool) -> bool {
    // Do not run interactive prompts on CI
    if std::env::var("GITHUB_ACTIONS").as_deref() == Ok("1") {
        return default_response;
    }

    println!("{prompt} [y/n]");
    read_line().to_lowercase() == "y"
}

pub fn read_line() -> String {
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .expect("cannot read line from stdin");
    line.trim().to_string()
}
