use std::process::{Command, Stdio};

/// Run a command from an array, collecting its output.
pub fn check_output<'a, Args: AsRef<[&'a str]>>(l: Args) -> String {
    let l = l.as_ref();
    check_output_cfg(l[0], |c| c.args(&l[1..]))
}

/// [`read`] with configuration. All shell helpers print the command and pass stderr.
fn check_output_cfg(prog: &str, f: impl FnOnce(&mut Command) -> &mut Command) -> String {
    let mut cmd = Command::new(prog);
    cmd.stderr(Stdio::inherit());
    f(&mut cmd);
    eprintln!("+ {cmd:?}");
    let out = cmd.output().expect("command failed");
    let stdout = String::from_utf8_lossy(out.stdout.trim_ascii()).to_string();
    if !out.status.success() {
        panic!(
            "Command `{cmd:?}` failed with exit code {:?}. STDOUT:\n{stdout}",
            out.status.code()
        );
    }
    stdout
}
