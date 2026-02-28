use std::ffi::OsStr;
use std::path::Path;

use codex_tui::ResumePickerRow;
use ignore::WalkBuilder;

const PROJECT_MAIN_FILE: &str = "MAIN.md";

pub fn discover_resumable_projects(workdir: &Path) -> anyhow::Result<Vec<ResumePickerRow>> {
    let projects_root = workdir.join(".codexpotter").join("projects");
    if !projects_root.is_dir() {
        return Ok(Vec::new());
    }

    let walker = WalkBuilder::new(&projects_root)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false)
        .parents(false)
        .follow_links(false)
        .build();

    let mut rows = Vec::new();

    for entry in walker {
        let entry = entry?;
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        if entry.path().file_name() != Some(OsStr::new(PROJECT_MAIN_FILE)) {
            continue;
        }

        match row_for_progress_file(workdir, entry.path())? {
            Some(row) => rows.push(row),
            None => continue,
        }
    }

    sort_rows(&mut rows);
    Ok(rows)
}

fn row_for_progress_file(
    workdir: &Path,
    progress_file: &Path,
) -> anyhow::Result<Option<ResumePickerRow>> {
    let resolved = match crate::resume::resolve_project_paths(workdir, progress_file) {
        Ok(resolved) => resolved,
        Err(_) => return Ok(None),
    };

    let potter_rollout_path = crate::potter_rollout::potter_rollout_path(&resolved.project_dir);
    if !potter_rollout_path.exists() || !potter_rollout_path.is_file() {
        return Ok(None);
    }

    let metadata = std::fs::metadata(&potter_rollout_path)?;
    let updated_at = metadata.modified()?;

    let potter_rollout_lines = crate::potter_rollout::read_lines(&potter_rollout_path)?;
    if potter_rollout_lines.is_empty() {
        return Ok(None);
    }

    let index = match crate::potter_rollout_resume_index::build_resume_index(&potter_rollout_lines)
    {
        Ok(index) => index,
        Err(_) => return Ok(None),
    };

    if !all_referenced_rollouts_exist(&resolved.workdir, &index) {
        return Ok(None);
    }

    let short_title = crate::project::progress_file_short_title(&resolved.progress_file)?;
    let git_branch = crate::project::progress_file_git_branch(&resolved.progress_file)?;

    let user_request = match short_title {
        Some(title) => title,
        None => index
            .session_started
            .user_message
            .clone()
            .unwrap_or_default(),
    };

    Ok(Some(ResumePickerRow {
        project_path: resolved.project_dir,
        user_request,
        updated_at,
        git_branch,
    }))
}

fn all_referenced_rollouts_exist(
    workdir: &Path,
    index: &crate::potter_rollout_resume_index::PotterRolloutResumeIndex,
) -> bool {
    let mut all_paths = Vec::new();
    for round in &index.completed_rounds {
        all_paths.push(&round.rollout_path);
    }
    if let Some(unfinished) = &index.unfinished_round {
        all_paths.push(&unfinished.rollout_path);
    }

    all_paths.into_iter().all(|rollout_path| {
        let resolved = if rollout_path.is_absolute() {
            rollout_path.to_path_buf()
        } else {
            workdir.join(rollout_path)
        };
        resolved.is_file()
    })
}

fn sort_rows(rows: &mut [ResumePickerRow]) {
    rows.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.project_path.cmp(&b.project_path))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;
    use std::time::Duration;
    use std::time::SystemTime;

    fn write_main(
        workdir: &Path,
        rel_dir: &str,
        short_title: Option<&str>,
        git_branch: Option<&str>,
    ) -> PathBuf {
        let path = workdir.join(rel_dir).join("MAIN.md");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");

        let short_title = short_title.unwrap_or("");
        let git_branch = git_branch.unwrap_or("");

        std::fs::write(
            &path,
            format!(
                r#"---
status: open
short_title: "{short_title}"
git_branch: "{git_branch}"
---

# Overall Goal
"#
            ),
        )
        .expect("write MAIN.md");

        path
    }

    fn write_resumable_potter_rollout(
        workdir: &Path,
        project_dir: &Path,
        user_message: Option<&str>,
        upstream_rollout_path: &Path,
    ) {
        std::fs::write(upstream_rollout_path, "").expect("write upstream rollout");

        let potter_rollout_path = project_dir.join(crate::potter_rollout::POTTER_ROLLOUT_FILENAME);
        let main_rel = project_dir
            .join("MAIN.md")
            .strip_prefix(workdir)
            .expect("strip_prefix")
            .to_path_buf();

        let thread_id =
            codex_protocol::ThreadId::from_string("019ca423-63d9-7641-ae83-db060ad3c000")
                .expect("thread id");

        crate::potter_rollout::append_line(
            &potter_rollout_path,
            &crate::potter_rollout::PotterRolloutLine::SessionStarted {
                user_message: user_message.map(ToOwned::to_owned),
                user_prompt_file: main_rel,
            },
        )
        .expect("append session_started");
        crate::potter_rollout::append_line(
            &potter_rollout_path,
            &crate::potter_rollout::PotterRolloutLine::RoundStarted {
                current: 1,
                total: 10,
            },
        )
        .expect("append round_started");
        crate::potter_rollout::append_line(
            &potter_rollout_path,
            &crate::potter_rollout::PotterRolloutLine::RoundConfigured {
                thread_id,
                rollout_path: upstream_rollout_path.to_path_buf(),
                rollout_path_raw: None,
                rollout_base_dir: None,
            },
        )
        .expect("append round_configured");
    }

    #[test]
    fn discover_finds_both_layout_styles_and_extracts_user_request() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workdir = temp.path();

        let main_a = write_main(
            workdir,
            ".codexpotter/projects/2026/02/28/1",
            Some("Short title A"),
            Some("main"),
        );
        write_resumable_potter_rollout(
            workdir,
            main_a.parent().expect("project dir"),
            Some("original prompt A"),
            &workdir.join("a.jsonl"),
        );

        let main_b = write_main(
            workdir,
            ".codexpotter/projects/20260228_1",
            None,
            Some("branch-b"),
        );
        write_resumable_potter_rollout(
            workdir,
            main_b.parent().expect("project dir"),
            Some("original prompt B"),
            &workdir.join("b.jsonl"),
        );

        let rows = discover_resumable_projects(workdir).expect("discover");
        assert_eq!(rows.len(), 2);

        let a_dir = main_a
            .canonicalize()
            .expect("canonicalize")
            .parent()
            .expect("parent")
            .to_path_buf();
        let b_dir = main_b
            .canonicalize()
            .expect("canonicalize")
            .parent()
            .expect("parent")
            .to_path_buf();

        let a = rows
            .iter()
            .find(|row| row.project_path == a_dir)
            .expect("a row");
        assert_eq!(a.user_request, "Short title A");
        assert_eq!(a.git_branch.as_deref(), Some("main"));

        let b = rows
            .iter()
            .find(|row| row.project_path == b_dir)
            .expect("b row");
        assert_eq!(b.user_request, "original prompt B");
        assert_eq!(b.git_branch.as_deref(), Some("branch-b"));
    }

    #[test]
    fn discover_excludes_non_resumable_candidates() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workdir = temp.path();

        let main_valid = write_main(
            workdir,
            ".codexpotter/projects/2026/02/28/1",
            Some("ok"),
            None,
        );
        write_resumable_potter_rollout(
            workdir,
            main_valid.parent().expect("project dir"),
            Some("prompt"),
            &workdir.join("valid.jsonl"),
        );

        // Missing potter-rollout.jsonl
        let _missing_rollout =
            write_main(workdir, ".codexpotter/projects/2026/02/28/2", None, None);

        // Empty potter-rollout.jsonl
        let main_empty = write_main(workdir, ".codexpotter/projects/2026/02/28/3", None, None);
        std::fs::write(
            main_empty
                .parent()
                .expect("project dir")
                .join(crate::potter_rollout::POTTER_ROLLOUT_FILENAME),
            "",
        )
        .expect("write empty rollout");

        // Missing referenced upstream rollout file
        let main_missing_upstream =
            write_main(workdir, ".codexpotter/projects/2026/02/28/4", None, None);
        let upstream_missing = workdir.join("missing-upstream.jsonl");
        let potter_rollout_path = main_missing_upstream
            .parent()
            .expect("project dir")
            .join(crate::potter_rollout::POTTER_ROLLOUT_FILENAME);
        let main_rel = main_missing_upstream
            .strip_prefix(workdir)
            .expect("strip_prefix")
            .to_path_buf();
        let thread_id =
            codex_protocol::ThreadId::from_string("019ca423-63d9-7641-ae83-db060ad3c000")
                .expect("thread id");
        crate::potter_rollout::append_line(
            &potter_rollout_path,
            &crate::potter_rollout::PotterRolloutLine::SessionStarted {
                user_message: Some("hello".to_string()),
                user_prompt_file: main_rel,
            },
        )
        .expect("append session_started");
        crate::potter_rollout::append_line(
            &potter_rollout_path,
            &crate::potter_rollout::PotterRolloutLine::RoundStarted {
                current: 1,
                total: 10,
            },
        )
        .expect("append round_started");
        crate::potter_rollout::append_line(
            &potter_rollout_path,
            &crate::potter_rollout::PotterRolloutLine::RoundConfigured {
                thread_id,
                rollout_path: upstream_missing,
                rollout_path_raw: None,
                rollout_base_dir: None,
            },
        )
        .expect("append round_configured");

        let rows = discover_resumable_projects(workdir).expect("discover");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].project_path,
            main_valid
                .canonicalize()
                .expect("canonicalize")
                .parent()
                .expect("parent")
                .to_path_buf()
        );
    }

    #[test]
    fn rows_sort_by_updated_desc_then_path() {
        let a = ResumePickerRow {
            project_path: PathBuf::from("/a"),
            user_request: String::new(),
            updated_at: SystemTime::UNIX_EPOCH + Duration::from_secs(10),
            git_branch: None,
        };
        let b = ResumePickerRow {
            project_path: PathBuf::from("/b"),
            user_request: String::new(),
            updated_at: SystemTime::UNIX_EPOCH + Duration::from_secs(20),
            git_branch: None,
        };
        let c = ResumePickerRow {
            project_path: PathBuf::from("/c"),
            user_request: String::new(),
            updated_at: SystemTime::UNIX_EPOCH + Duration::from_secs(20),
            git_branch: None,
        };

        let mut rows = vec![a.clone(), b.clone(), c.clone()];
        sort_rows(&mut rows);
        assert_eq!(rows, vec![b, c, a]);
    }
}
