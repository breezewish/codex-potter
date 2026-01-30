use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;

pub fn ensure_default_codex_compat_home() -> anyhow::Result<Option<PathBuf>> {
    let Some(home) = dirs::home_dir() else {
        return Ok(None);
    };
    ensure_codex_compat_home(&home).map(Some)
}

fn ensure_codex_compat_home(home: &Path) -> anyhow::Result<PathBuf> {
    let codex_home = home.join(".codexpotter").join("codex-compat");
    std::fs::create_dir_all(&codex_home)
        .with_context(|| format!("create directory {}", codex_home.display()))?;

    ensure_symlink(
        &codex_home.join("config.toml"),
        &home.join(".codex").join("config.toml"),
    )?;
    ensure_symlink(
        &codex_home.join("auth.json"),
        &home.join(".codex").join("auth.json"),
    )?;

    Ok(codex_home)
}

fn ensure_symlink(link_path: &Path, target_path: &Path) -> anyhow::Result<()> {
    if std::fs::symlink_metadata(link_path).is_ok() {
        return Ok(());
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target_path, link_path)
            .with_context(|| format!("create symlink {}", link_path.display()))?;
        Ok(())
    }

    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target_path, link_path)
            .with_context(|| format!("create symlink {}", link_path.display()))?;
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    anyhow::bail!("symlinks are not supported on this platform");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn ensures_codex_compat_home_and_links() {
        let home_dir = tempfile::tempdir().expect("tempdir");
        let codex_home = ensure_codex_compat_home(home_dir.path()).expect("ensure home");

        assert!(codex_home.is_dir());

        let config_link = codex_home.join("config.toml");
        let auth_link = codex_home.join("auth.json");

        let config_meta = std::fs::symlink_metadata(&config_link).expect("config symlink");
        assert!(config_meta.file_type().is_symlink());
        let auth_meta = std::fs::symlink_metadata(&auth_link).expect("auth symlink");
        assert!(auth_meta.file_type().is_symlink());

        // Running it again should be a no-op (even if the targets are missing).
        let codex_home_again =
            ensure_codex_compat_home(home_dir.path()).expect("ensure home again");
        assert_eq!(codex_home_again, codex_home);
    }
}
