use regex::Regex;
use std::borrow::Cow;
use std::path::Path;
use std::process::{Command, Stdio};

/// Run command and return its stdout.
pub fn run_command<'a, Args: AsRef<[&'a str]>>(args: Args) -> anyhow::Result<String> {
    run_command_at(args, &std::env::current_dir()?, StderrMode::Print)
}

pub enum StderrMode {
    Ignore,
    Print,
}

pub fn run_command_at<'a, Args: AsRef<[&'a str]>>(
    args: Args,
    workdir: &Path,
    stderr: StderrMode,
) -> anyhow::Result<String> {
    let args = args.as_ref();

    let mut cmd = Command::new(args[0]);
    cmd.current_dir(workdir);
    cmd.args(&args[1..]);

    if matches!(stderr, StderrMode::Ignore) {
        cmd.stderr(Stdio::null());
    }
    eprintln!("+ {cmd:?}");
    let out = cmd.output().expect("command failed");
    let stdout = String::from_utf8_lossy(out.stdout.trim_ascii()).to_string();
    if !out.status.success() {
        Err(anyhow::anyhow!(
            "Command `{cmd:?}` failed with exit code {:?}. STDOUT:\n{stdout}",
            out.status.code()
        ))
    } else {
        Ok(stdout)
    }
}

/// Fail if there are files that need to be checked in.
pub fn ensure_clean_git_state() {
    let read = run_command(["git", "status", "--untracked-files=no", "--porcelain"])
        .expect("cannot figure out if git state is clean");
    assert!(read.is_empty(), "working directory must be clean");
}

/// Ask a prompt to user and return true if they responded with `y`.
pub fn prompt(prompt: &str) -> bool {
    // Do not run interactive prompts on CI
    if std::env::var("GITHUB_ACTIONS").as_deref() == Ok("1") {
        return false;
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

/// Replace `#1234`-style issue/PR references with `repo#1234` to ensure links work across
/// repositories.
pub fn replace_references<'a>(text: &'a str, repo: &str) -> Cow<'a, str> {
    let regex = Regex::new(r"\B(?P<id>#\d+)\b").unwrap();
    regex.replace(text, &format!("{repo}$id"))
}
