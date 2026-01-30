use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Context;
use chrono::DateTime;
use chrono::Local;

const PROJECT_MAIN_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/project_main.md"
));
const DEVELOPER_PROMPT_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/developer_prompt.md"
));
const PROMPT_TEMPLATE: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prompts/prompt.md"));

#[derive(Debug, Clone)]
pub struct ProjectInit {
    pub progress_file_rel: PathBuf,
}

pub fn init_project(
    workdir: &Path,
    user_prompt: &str,
    now: DateTime<Local>,
) -> anyhow::Result<ProjectInit> {
    let (git_commit, git_branch) = resolve_git_metadata(workdir);

    let codexpotter_dir = workdir.join(".codexpotter");
    let projects_root = codexpotter_dir.join("projects");
    let kb_dir = codexpotter_dir.join("kb");

    std::fs::create_dir_all(&projects_root)
        .with_context(|| format!("create {}", projects_root.display()))?;
    std::fs::create_dir_all(&kb_dir).with_context(|| format!("create {}", kb_dir.display()))?;

    let date = now.format("%Y%m%d").to_string();
    let (project_dir, progress_file_rel) = create_next_project_dir(&projects_root, &date)?;

    let main_md = project_dir.join("MAIN.md");
    let main_md_contents = render_project_main(user_prompt, &git_commit, &git_branch);
    std::fs::write(&main_md, main_md_contents)
        .with_context(|| format!("write {}", main_md.display()))?;

    Ok(ProjectInit { progress_file_rel })
}

pub fn render_project_main(user_prompt: &str, git_commit: &str, git_branch: &str) -> String {
    let git_commit = yaml_escape_double_quoted(git_commit);
    let git_branch = yaml_escape_double_quoted(git_branch);

    PROJECT_MAIN_TEMPLATE
        .replace("{{USER_PROMPT}}", user_prompt)
        .replace("{{GIT_COMMIT}}", &git_commit)
        .replace("{{GIT_BRANCH}}", &git_branch)
}

pub fn render_developer_prompt(progress_file_rel: &Path) -> String {
    let progress_file_rel = progress_file_rel.to_string_lossy();
    DEVELOPER_PROMPT_TEMPLATE.replace("{{PROGRESS_FILE}}", &progress_file_rel)
}

pub fn fixed_prompt() -> &'static str {
    PROMPT_TEMPLATE
}

pub fn progress_file_has_potterflag_true(
    workdir: &Path,
    progress_file_rel: &Path,
) -> anyhow::Result<bool> {
    let progress_file = workdir.join(progress_file_rel);
    let contents = std::fs::read_to_string(&progress_file)
        .with_context(|| format!("read {}", progress_file.display()))?;
    Ok(front_matter_bool(&contents, "potterflag").unwrap_or(false))
}

fn create_next_project_dir(projects_root: &Path, date: &str) -> anyhow::Result<(PathBuf, PathBuf)> {
    for idx in 1.. {
        let name = format!("{date}_{idx}");
        let project_dir = projects_root.join(&name);
        if project_dir.exists() {
            continue;
        }

        std::fs::create_dir_all(&project_dir)
            .with_context(|| format!("create {}", project_dir.display()))?;

        let progress_file_rel = PathBuf::from(".codexpotter")
            .join("projects")
            .join(name)
            .join("MAIN.md");
        return Ok((project_dir, progress_file_rel));
    }

    unreachable!("project index overflow");
}

fn front_matter_bool(contents: &str, key: &str) -> Option<bool> {
    let mut lines = contents.lines();
    let first = lines.next()?.trim_end();
    if first != "---" {
        return None;
    }

    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((k, v)) = trimmed.split_once(':') else {
            continue;
        };
        if k.trim() != key {
            continue;
        }

        let raw = v.trim();
        let first_token = raw.split_whitespace().next().unwrap_or_default();
        let unquoted = first_token.trim_matches(&['"', '\''][..]);
        return Some(unquoted.eq_ignore_ascii_case("true"));
    }

    None
}

fn resolve_git_metadata(workdir: &Path) -> (String, String) {
    let git_commit = git_stdout_trimmed(workdir, &["rev-parse", "HEAD"]).unwrap_or_default();
    let git_branch =
        git_stdout_trimmed(workdir, &["symbolic-ref", "-q", "--short", "HEAD"]).unwrap_or_default();

    (git_commit, git_branch)
}

fn git_stdout_trimmed(workdir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return None;
    }

    Some(stdout)
}

fn yaml_escape_double_quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;
    use std::process::Command;

    #[test]
    fn init_project_creates_main_md_and_increments_suffix() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Local
            .with_ymd_and_hms(2026, 1, 27, 12, 0, 0)
            .single()
            .expect("timestamp");

        let first = init_project(temp.path(), "do something", now).expect("init project");
        assert_eq!(
            first.progress_file_rel,
            PathBuf::from(".codexpotter/projects/20260127_1/MAIN.md")
        );

        let kb_dir = temp.path().join(".codexpotter/kb");
        assert!(kb_dir.exists());

        let first_main = temp.path().join(&first.progress_file_rel);
        assert!(first_main.exists());

        let main = std::fs::read_to_string(&first_main).expect("read main");
        assert!(main.contains("# Overall Goal"));
        assert!(main.contains("do something"));
        assert!(main.contains("git_commit: \"\""));
        assert!(main.contains("git_branch: \"\""));

        let second = init_project(temp.path(), "do something else", now).expect("init project");
        assert_eq!(
            second.progress_file_rel,
            PathBuf::from(".codexpotter/projects/20260127_2/MAIN.md")
        );

        let second_main = temp.path().join(&second.progress_file_rel);
        assert!(second_main.exists());

        let developer = render_developer_prompt(&second.progress_file_rel);
        assert!(developer.contains(".codexpotter/projects/20260127_2/MAIN.md"));
    }

    #[test]
    fn init_project_writes_git_commit_and_branch_when_in_repo() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }

        let temp = tempfile::tempdir().expect("tempdir");

        let workdir = temp.path();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(workdir)
                .args(["init", "-q"])
                .status()
                .expect("git init")
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(workdir)
                .args(["config", "user.name", "test"])
                .status()
                .expect("git config user.name")
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(workdir)
                .args(["config", "user.email", "test@example.com"])
                .status()
                .expect("git config user.email")
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(workdir)
                .args(["checkout", "-q", "-b", "test-branch"])
                .status()
                .expect("git checkout -b")
                .success()
        );

        std::fs::write(workdir.join("README.md"), "hello\n").expect("write file");
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(workdir)
                .args(["add", "."])
                .status()
                .expect("git add")
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(workdir)
                .args(["commit", "-q", "-m", "init"])
                .status()
                .expect("git commit")
                .success()
        );

        let git_commit = git_stdout_trimmed(workdir, &["rev-parse", "HEAD"]).expect("rev-parse");
        let git_branch = git_stdout_trimmed(workdir, &["symbolic-ref", "-q", "--short", "HEAD"])
            .expect("branch");
        assert_eq!(git_branch, "test-branch");

        let now = Local
            .with_ymd_and_hms(2026, 1, 27, 12, 0, 0)
            .single()
            .expect("timestamp");
        let init = init_project(workdir, "do something", now).expect("init project");

        let main = std::fs::read_to_string(workdir.join(&init.progress_file_rel)).expect("read");
        assert!(main.contains(&format!("git_commit: \"{git_commit}\"")));
        assert!(main.contains("git_branch: \"test-branch\""));

        assert!(
            Command::new("git")
                .arg("-C")
                .arg(workdir)
                .args(["checkout", "-q", "--detach"])
                .status()
                .expect("git checkout --detach")
                .success()
        );

        let detached = init_project(workdir, "do something else", now).expect("init detached");
        let main =
            std::fs::read_to_string(workdir.join(&detached.progress_file_rel)).expect("read");
        assert!(main.contains(&format!("git_commit: \"{git_commit}\"")));
        assert!(main.contains("git_branch: \"\""));
    }

    #[test]
    fn progress_file_has_potterflag_true_reads_front_matter() {
        let temp = tempfile::tempdir().expect("tempdir");
        let progress = temp.path().join("MAIN.md");
        std::fs::write(
            &progress,
            r#"---
status: open
potterflag: true
---

# Overall Goal
"#,
        )
        .expect("write progress file");

        let rel = PathBuf::from("MAIN.md");
        let flagged =
            progress_file_has_potterflag_true(temp.path(), &rel).expect("read potterflag");
        assert!(flagged);
    }

    #[test]
    fn progress_file_has_potterflag_true_is_false_when_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let progress = temp.path().join("MAIN.md");
        std::fs::write(
            &progress,
            r#"---
status: open
---

# Overall Goal
"#,
        )
        .expect("write progress file");

        let rel = PathBuf::from("MAIN.md");
        let flagged =
            progress_file_has_potterflag_true(temp.path(), &rel).expect("read potterflag");
        assert!(!flagged);
    }
}
