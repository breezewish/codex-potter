use std::ffi::OsStr;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;

const PROJECT_MAIN_FILE: &str = "MAIN.md";
const CODEXPOTTER_DIR: &str = ".codexpotter";

#[derive(Debug, Clone, PartialEq, Eq)]
/// Canonicalized paths derived from a user-provided `PROJECT_PATH`.
pub struct ResolvedProjectPaths {
    pub progress_file: PathBuf,
    pub project_dir: PathBuf,
    pub workdir: PathBuf,
}

/// Resolve a user-supplied project path into a unique `MAIN.md` progress file, plus derived dirs.
///
/// Supported input forms include:
/// - `2026/02/01/1`
/// - `.codexpotter/projects/2026/02/01/1`
/// - `/abs/path/to/.codexpotter/projects/2026/02/01/1`
/// - any of the above with `/MAIN.md` suffix
pub fn resolve_project_paths(cwd: &Path, project_path: &Path) -> anyhow::Result<ResolvedProjectPaths> {
    let project_path = crate::path_utils::expand_tilde(project_path);
    let candidates = build_candidate_progress_files(cwd, &project_path);

    let mut found: Vec<PathBuf> = Vec::new();
    let mut tried: Vec<PathBuf> = Vec::new();
    for candidate in candidates {
        tried.push(candidate.clone());
        if candidate.is_file() {
            let canonical = candidate
                .canonicalize()
                .with_context(|| format!("canonicalize {}", candidate.display()))?;
            if !found.contains(&canonical) {
                found.push(canonical);
            }
        }
    }

    let progress_file = match found.len() {
        0 => {
            let tried = tried
                .into_iter()
                .map(|path| format!("- {}", crate::path_utils::display_with_tilde(&path)))
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::bail!("no progress file found for project path. tried:\n{tried}");
        }
        1 => found
            .pop()
            .context("pop single resolved progress file")?,
        _ => {
            let candidates = found
                .into_iter()
                .map(|path| format!("- {}", crate::path_utils::display_with_tilde(&path)))
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::bail!("ambiguous project path. candidates:\n{candidates}");
        }
    };

    let project_dir = progress_file
        .parent()
        .context("derive project_dir from progress_file")?
        .to_path_buf();

    let workdir = derive_project_workdir(&progress_file)?;

    Ok(ResolvedProjectPaths {
        progress_file,
        project_dir,
        workdir,
    })
}

fn build_candidate_progress_files(cwd: &Path, project_path: &Path) -> Vec<PathBuf> {
    if project_path.is_absolute() {
        return vec![ensure_main_md(project_path.to_path_buf())];
    }

    let a = cwd
        .join(CODEXPOTTER_DIR)
        .join("projects")
        .join(project_path);
    let b = cwd.join(project_path);

    vec![ensure_main_md(a), ensure_main_md(b)]
}

fn ensure_main_md(path: PathBuf) -> PathBuf {
    let is_main_md = path.file_name() == Some(OsStr::new(PROJECT_MAIN_FILE));
    if is_main_md {
        return path;
    }
    path.join(PROJECT_MAIN_FILE)
}

fn derive_project_workdir(progress_file: &Path) -> anyhow::Result<PathBuf> {
    let mut current = progress_file
        .parent()
        .context("progress file has no parent directory")?;

    loop {
        if current.file_name() == Some(OsStr::new(CODEXPOTTER_DIR)) {
            return current
                .parent()
                .context("derive project workdir from .codexpotter parent")?
                .to_path_buf()
                .canonicalize()
                .context("canonicalize project workdir");
        }

        current = current.parent().with_context(|| {
            format!(
                "progress file is not inside a `{CODEXPOTTER_DIR}` directory: {}",
                progress_file.display()
            )
        })?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn write_main(root: &Path, rel: &str) -> PathBuf {
        let path = root.join(rel).join("MAIN.md");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "---\nstatus: open\n---\n").expect("write MAIN.md");
        path
    }

    #[test]
    fn resolve_project_paths_supports_relative_short_form() {
        let temp = tempfile::tempdir().expect("tempdir");
        let main = write_main(
            temp.path(),
            ".codexpotter/projects/2026/02/01/1",
        );

        let resolved = resolve_project_paths(temp.path(), Path::new("2026/02/01/1"))
            .expect("resolve");

        assert_eq!(resolved.progress_file, main.canonicalize().expect("canonical"));
        assert_eq!(
            resolved.project_dir,
            main.canonicalize()
                .expect("canonical")
                .parent()
                .expect("project_dir")
                .to_path_buf()
        );
        assert_eq!(
            resolved.workdir,
            temp.path().canonicalize().expect("canonical")
        );
    }

    #[test]
    fn resolve_project_paths_accepts_absolute_project_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let main = write_main(
            temp.path(),
            ".codexpotter/projects/2026/02/01/1",
        );
        let project_dir = main.parent().expect("project dir");

        let resolved = resolve_project_paths(temp.path(), project_dir).expect("resolve");
        assert_eq!(resolved.progress_file, main.canonicalize().expect("canonical"));
    }

    #[test]
    fn resolve_project_paths_errors_when_ambiguous() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _a = write_main(temp.path(), ".codexpotter/projects/foo");
        let _b = write_main(temp.path(), "foo");

        let err = resolve_project_paths(temp.path(), Path::new("foo"))
            .expect_err("expected ambiguity error");
        let message = format!("{err:#}");
        assert!(
            message.contains("ambiguous project path"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn resolve_project_paths_lists_tried_paths_on_missing() {
        let temp = tempfile::tempdir().expect("tempdir");

        let err = resolve_project_paths(temp.path(), Path::new("missing"))
            .expect_err("expected missing error");
        let message = format!("{err:#}");
        assert!(
            message.contains("no progress file found"),
            "unexpected error: {message}"
        );
        assert!(message.contains(".codexpotter/projects/missing/MAIN.md"));
        assert!(message.contains("missing/MAIN.md"));
    }
}
