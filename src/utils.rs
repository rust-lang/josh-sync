use std::process::{Command, Stdio};

/// Run a command from an array, collecting its output.
pub fn check_output<'a, Args: AsRef<[&'a str]>>(l: Args) -> anyhow::Result<String> {
    let args = l.as_ref();

    let mut cmd = Command::new(args[0]);
    cmd.args(&args[1..]);
    cmd.stderr(Stdio::inherit());
    eprintln!("+ {cmd:?}");
    let out = cmd.output().expect("command failed");
    let stdout = String::from_utf8_lossy(out.stdout.trim_ascii()).to_string();
    if !out.status.success() {
        panic!(
            "Command `{cmd:?}` failed with exit code {:?}. STDOUT:\n{stdout}",
            out.status.code()
        );
    }
    Ok(stdout)
}

/// Fail if there are files that need to be checked in.
pub fn ensure_clean_git_state() {
    let read = check_output(["git", "status", "--untracked-files=no", "--porcelain"])
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
