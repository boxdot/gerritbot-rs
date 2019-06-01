use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct VersionInfo {
    pub package_name: &'static str,
    pub package_version: &'static str,
    pub git_commit_id: &'static str,
    pub ci_commit_id: Option<&'static str>,
    pub target_triple: &'static str,
    pub build_date: &'static str,
    pub rustc_version: &'static str,
}

pub const VERSION_INFO: VersionInfo = VersionInfo {
    package_name: env!("CARGO_PKG_NAME"),
    package_version: env!("CARGO_PKG_VERSION"),
    git_commit_id: env!("VERGEN_SHA"),
    ci_commit_id: option_env!("CI_COMMIT_SHA"),
    target_triple: env!("VERGEN_TARGET_TRIPLE"),
    build_date: env!("VERGEN_BUILD_DATE"),
    rustc_version: env!("GERRITBOT_RUSTC_VERSION"),
};
