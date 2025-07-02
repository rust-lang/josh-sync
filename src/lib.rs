#[derive(serde::Serialize, serde::Deserialize)]
pub struct JoshConfig {
    #[serde(default = "default_org")]
    pub org: String,
    pub repo: String,
    /// Last SHA of rust-lang/rust that was pulled into this subtree.
    #[serde(default)]
    pub upstream_sha: Option<String>
}

fn default_org() -> String {
    String::from("rust-lang")
}
