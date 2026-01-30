use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use ignore::gitignore::GitignoreBuilder;

use crate::atomic_write::write_atomic_text;
use crate::path_utils;

pub const CODEXPOTTER_GITIGNORE_ENTRY: &str = ".codexpotter/";

#[derive(Debug, Clone)]
pub struct GlobalGitignoreStatus {
    pub path: PathBuf,
    pub path_display: String,
    pub has_codexpotter_ignore: bool,
}

pub fn detect_global_gitignore(workdir: &Path) -> anyhow::Result<GlobalGitignoreStatus> {
    let path = resolve_global_gitignore_path()?;
    let path_display = path_utils::display_with_tilde(&path);
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == ErrorKind::NotFound => String::new(),
        Err(err) => return Err(anyhow::Error::new(err).context("read global gitignore")),
    };
    let has_codexpotter_ignore = gitignore_ignores_codexpotter(workdir, &contents);

    Ok(GlobalGitignoreStatus {
        path,
        path_display,
        has_codexpotter_ignore,
    })
}

pub fn ensure_codexpotter_ignored(workdir: &Path, path: &Path) -> anyhow::Result<()> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == ErrorKind::NotFound => String::new(),
        Err(err) => return Err(anyhow::Error::new(err).context("read global gitignore")),
    };

    if gitignore_ignores_codexpotter(workdir, &contents) {
        return Ok(());
    }

    let mut updated = contents;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(CODEXPOTTER_GITIGNORE_ENTRY);
    updated.push('\n');

    write_atomic_text(path, &updated)
}

fn resolve_global_gitignore_path() -> anyhow::Result<PathBuf> {
    if let Ok(output) = Command::new("git")
        .args(["config", "--global", "--path", "--get", "core.excludesfile"])
        .output()
        && output.status.success()
    {
        let configured = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !configured.is_empty() {
            return Ok(path_utils::expand_tilde(Path::new(&configured)));
        }
    }

    let Some(home) = dirs::home_dir() else {
        anyhow::bail!("cannot determine home directory for global gitignore path");
    };

    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    Ok(match xdg {
        Some(base) => base.join("git").join("ignore"),
        None => home.join(".config").join("git").join("ignore"),
    })
}

fn gitignore_ignores_codexpotter(workdir: &Path, contents: &str) -> bool {
    let mut builder = GitignoreBuilder::new(workdir);
    for line in contents.lines() {
        if builder.add_line(None, line).is_err() {
            continue;
        }
    }

    let gitignore = match builder.build() {
        Ok(gitignore) => gitignore,
        Err(_) => return false,
    };

    let codexpotter_dir = Path::new(".codexpotter");
    if gitignore.matched(codexpotter_dir, true).is_ignore() {
        return true;
    }

    let codexpotter_placeholder = codexpotter_dir.join("placeholder");
    gitignore
        .matched(&codexpotter_placeholder, false)
        .is_ignore()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_codexpotter_ignore_patterns() {
        let dir = tempfile::tempdir().expect("tempdir");

        assert!(gitignore_ignores_codexpotter(dir.path(), ".codexpotter/\n"));
        assert!(gitignore_ignores_codexpotter(
            dir.path(),
            "**/.codexpotter/\n"
        ));
        assert!(gitignore_ignores_codexpotter(dir.path(), ".codexpotter\n"));
        assert!(gitignore_ignores_codexpotter(
            dir.path(),
            ".codexpotter/**\n"
        ));

        assert!(!gitignore_ignores_codexpotter(
            dir.path(),
            ".codexpotter/\n!.codexpotter/\n"
        ));
        assert!(!gitignore_ignores_codexpotter(
            dir.path(),
            "# .codexpotter/\n"
        ));
        assert!(!gitignore_ignores_codexpotter(
            dir.path(),
            "!.codexpotter/\n"
        ));
        assert!(!gitignore_ignores_codexpotter(
            dir.path(),
            ".codexpotter-old/\n"
        ));
    }
}
