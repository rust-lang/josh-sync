use crate::utils::check_output;
use std::path::PathBuf;

pub struct JoshProxy {
    path: PathBuf,
}

impl JoshProxy {
    /// Tries to figure out if `josh-proxy` is installed.
    pub fn lookup() -> Option<Self> {
        which::which("josh-proxy").ok().map(|path| Self { path })
    }
}

pub fn try_install_josh() -> Option<JoshProxy> {
    check_output(&[
        "cargo",
        "install",
        "--locked",
        "--git",
        "https://github.com/josh-project/josh",
        "--tag",
        "r24.10.04",
        "josh-proxy",
    ]);
    JoshProxy::lookup()
}
