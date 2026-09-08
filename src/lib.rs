use crate::config::JoshConfig;

pub mod config;
pub mod josh;
pub mod sync;
pub mod utils;

#[derive(Clone)]
pub struct SyncContext {
    pub config: JoshConfig,
    /// The last synced upstream SHA, which should be present
    /// if a pull was already performed at least once.
    pub last_upstream_sha: Option<String>,
}
