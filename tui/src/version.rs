/// The CodexPotter CLI version.
///
/// In development builds, this defaults to the workspace Cargo package version.
/// In release builds, GitHub Actions injects the tag version via the
/// `CODEX_POTTER_VERSION` environment variable so the CLI can be released by
/// tagging without editing `Cargo.toml`.
///
/// # Divergence from upstream Codex CLI
///
/// Upstream Codex CLI uses a different version/tag scheme; CodexPotter's version can be injected
/// at build time to match Git tags.
pub const CODEX_POTTER_VERSION: &str = match option_env!("CODEX_POTTER_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
