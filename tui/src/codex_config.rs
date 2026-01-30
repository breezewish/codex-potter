use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use codex_protocol::openai_models::ReasoningEffort;
use toml_edit::DocumentMut;
use toml_edit::Item as TomlItem;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexModelConfig {
    pub model: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub profile: Option<String>,
    pub profiles: HashMap<String, CodexProfileModelConfig>,
    pub project_root_markers: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexProfileModelConfig {
    pub model: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCodexModelConfig {
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
}

pub fn resolve_codex_model_config(cwd: &Path) -> io::Result<ResolvedCodexModelConfig> {
    let raw = load_codex_model_config(cwd)?;

    let profile_config = match &raw.profile {
        Some(name) => raw.profiles.get(name).cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("config profile `{name}` not found"),
            )
        })?,
        None => CodexProfileModelConfig::default(),
    };

    let model = profile_config
        .model
        .or(raw.model)
        .unwrap_or_else(|| DEFAULT_FALLBACK_MODEL.to_string());
    let reasoning_effort = profile_config.reasoning_effort.or(raw.reasoning_effort);

    Ok(ResolvedCodexModelConfig {
        model,
        reasoning_effort,
    })
}

const DEFAULT_FALLBACK_MODEL: &str = "gpt-5.2-codex";

fn load_codex_model_config(cwd: &Path) -> io::Result<CodexModelConfig> {
    let codex_home = find_codex_home()?;
    let mut config = CodexModelConfig::default();

    // Match codex config layering order (subset):
    // - system: /etc/codex/config.toml
    // - user:   $CODEX_HOME/config.toml (default ~/.codex/config.toml)
    // - project layers: ./.../.codex/config.toml from project root to cwd
    apply_config_layer_from_file(&mut config, &default_system_config_path())?;
    apply_config_layer_from_file(&mut config, &codex_home.join("config.toml"))?;

    let project_root_markers = config
        .project_root_markers
        .clone()
        .unwrap_or_else(default_project_root_markers);
    let project_root = find_project_root(cwd, &project_root_markers)?;
    for dir in project_dirs_between(&project_root, cwd) {
        let dot_codex = dir.join(".codex");
        if !dot_codex.is_dir() {
            continue;
        }
        apply_config_layer_from_file(&mut config, &dot_codex.join("config.toml"))?;
    }

    Ok(config)
}

fn default_project_root_markers() -> Vec<String> {
    vec![".git".to_string()]
}

fn default_system_config_path() -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from("/etc/codex/config.toml")
    }
    #[cfg(not(unix))]
    {
        PathBuf::new()
    }
}

fn find_codex_home() -> io::Result<PathBuf> {
    if let Ok(val) = std::env::var("CODEX_HOME")
        && !val.is_empty()
    {
        return PathBuf::from(val).canonicalize();
    }

    let mut p = dirs::home_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Could not find home directory"))?;
    p.push(".codex");
    Ok(p)
}

fn find_project_root(cwd: &Path, project_root_markers: &[String]) -> io::Result<PathBuf> {
    if project_root_markers.is_empty() {
        return Ok(cwd.to_path_buf());
    }

    for ancestor in cwd.ancestors() {
        for marker in project_root_markers {
            let marker_path = ancestor.join(marker);
            if std::fs::metadata(&marker_path).is_ok() {
                return Ok(ancestor.to_path_buf());
            }
        }
    }

    Ok(cwd.to_path_buf())
}

fn project_dirs_between<'a>(project_root: &'a Path, cwd: &'a Path) -> Vec<&'a Path> {
    let mut dirs = cwd
        .ancestors()
        .scan(false, |done, ancestor| {
            if *done {
                None
            } else {
                if ancestor == project_root {
                    *done = true;
                }
                Some(ancestor)
            }
        })
        .collect::<Vec<_>>();
    dirs.reverse();
    dirs
}

fn apply_config_layer_from_file(config: &mut CodexModelConfig, path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }

    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(io::Error::new(
                err.kind(),
                format!("Failed to read config file {}: {err}", path.display()),
            ));
        }
    };

    let doc = contents.parse::<DocumentMut>().map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Error parsing config file {}: {err}", path.display()),
        )
    })?;

    apply_config_layer_from_doc(config, &doc)
}

fn apply_config_layer_from_doc(config: &mut CodexModelConfig, doc: &DocumentMut) -> io::Result<()> {
    if let Some(item) = doc.get("model") {
        config.model = Some(read_string(item, "model")?);
    }
    if let Some(item) = doc.get("model_reasoning_effort") {
        config.reasoning_effort = Some(read_reasoning_effort(item, "model_reasoning_effort")?);
    }
    if let Some(item) = doc.get("profile") {
        config.profile = Some(read_string(item, "profile")?);
    }
    if let Some(item) = doc.get("project_root_markers") {
        config.project_root_markers = Some(read_string_array(item, "project_root_markers")?);
    }

    if let Some(profiles_item) = doc.get("profiles") {
        let profiles_table = profiles_item.as_table().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "config field `profiles` must be a table",
            )
        })?;
        for (profile_name, profile_item) in profiles_table.iter() {
            let profile_table = profile_item.as_table().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("config field `profiles.{profile_name}` must be a table"),
                )
            })?;

            let mut profile = config.profiles.remove(profile_name).unwrap_or_default();
            if let Some(item) = profile_table.get("model") {
                profile.model = Some(read_string(
                    item,
                    &format!("profiles.{profile_name}.model"),
                )?);
            }
            if let Some(item) = profile_table.get("model_reasoning_effort") {
                profile.reasoning_effort = Some(read_reasoning_effort(
                    item,
                    &format!("profiles.{profile_name}.model_reasoning_effort"),
                )?);
            }
            config.profiles.insert(profile_name.to_string(), profile);
        }
    }

    Ok(())
}

fn read_string(item: &TomlItem, field: &str) -> io::Result<String> {
    item.as_value()
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("config field `{field}` must be a string"),
            )
        })
}

fn read_string_array(item: &TomlItem, field: &str) -> io::Result<Vec<String>> {
    let array = item.as_array().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("config field `{field}` must be an array of strings"),
        )
    })?;

    let mut out: Vec<String> = Vec::new();
    for value in array.iter() {
        let s = value.as_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("config field `{field}` must be an array of strings"),
            )
        })?;
        out.push(s.to_string());
    }

    Ok(out)
}

fn read_reasoning_effort(item: &TomlItem, field: &str) -> io::Result<ReasoningEffort> {
    let raw = read_string(item, field)?;
    parse_reasoning_effort(&raw).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("config field `{field}` has invalid value `{raw}`"),
        )
    })
}

fn parse_reasoning_effort(value: &str) -> Option<ReasoningEffort> {
    match value {
        "none" => Some(ReasoningEffort::None),
        "minimal" => Some(ReasoningEffort::Minimal),
        "low" => Some(ReasoningEffort::Low),
        "medium" => Some(ReasoningEffort::Medium),
        "high" => Some(ReasoningEffort::High),
        "xhigh" => Some(ReasoningEffort::XHigh),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    struct EnvVarGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let prev = std::env::var_os(key);
            // Safety: tests are serialized and restore the previous value on drop.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, prev }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(value) => unsafe {
                    std::env::set_var(self.key, value);
                },
                None => unsafe {
                    std::env::remove_var(self.key);
                },
            }
        }
    }

    fn write_config(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir parent");
        std::fs::write(path, contents).expect("write config");
    }

    #[test]
    #[serial]
    fn resolves_model_from_profile_when_selected() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let _env = EnvVarGuard::set("CODEX_HOME", codex_home.path());

        write_config(
            &codex_home.path().join("config.toml"),
            r#"
model = "gpt-5.2"
model_reasoning_effort = "xhigh"
profile = "work"

[profiles.work]
model = "gpt-5.2-codex"
model_reasoning_effort = "high"
"#,
        );

        let cwd = tempfile::tempdir().expect("cwd");
        let resolved = resolve_codex_model_config(cwd.path()).expect("resolve");
        assert_eq!(resolved.model, "gpt-5.2-codex");
        assert_eq!(resolved.reasoning_effort, Some(ReasoningEffort::High));
    }

    #[test]
    #[serial]
    fn project_layer_overrides_user_layer() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let _env = EnvVarGuard::set("CODEX_HOME", codex_home.path());

        write_config(
            &codex_home.path().join("config.toml"),
            r#"
model = "gpt-5.2"
"#,
        );

        let repo = tempfile::tempdir().expect("repo");
        std::fs::create_dir_all(repo.path().join(".git")).expect("mkdir .git");
        std::fs::create_dir_all(repo.path().join(".codex")).expect("mkdir .codex");
        write_config(
            &repo.path().join(".codex").join("config.toml"),
            r#"
model = "gpt-5.2-codex"
"#,
        );

        let resolved = resolve_codex_model_config(repo.path()).expect("resolve");
        assert_eq!(resolved.model, "gpt-5.2-codex");
    }

    #[test]
    #[serial]
    fn resolving_selected_profile_errors_when_profile_is_missing() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let _env = EnvVarGuard::set("CODEX_HOME", codex_home.path());

        write_config(
            &codex_home.path().join("config.toml"),
            r#"
model = "gpt-5.2"
profile = "missing"
"#,
        );

        let cwd = tempfile::tempdir().expect("cwd");
        let err = resolve_codex_model_config(cwd.path()).expect_err("expected error");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(
            err.to_string()
                .contains("config profile `missing` not found"),
            "unexpected error: {err}",
        );
    }

    #[test]
    #[serial]
    fn project_root_markers_can_change_project_root_discovery() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let _env = EnvVarGuard::set("CODEX_HOME", codex_home.path());

        write_config(
            &codex_home.path().join("config.toml"),
            r#"
model = "gpt-5.2"
project_root_markers = ["MARKER"]
"#,
        );

        let repo = tempfile::tempdir().expect("repo");
        std::fs::write(repo.path().join("MARKER"), "").expect("write marker");
        std::fs::create_dir_all(repo.path().join(".codex")).expect("mkdir .codex");
        write_config(
            &repo.path().join(".codex").join("config.toml"),
            r#"
model = "gpt-5.2-codex"
"#,
        );

        let cwd = repo.path().join("subdir");
        std::fs::create_dir_all(&cwd).expect("mkdir subdir");

        let resolved = resolve_codex_model_config(&cwd).expect("resolve");
        assert_eq!(resolved.model, "gpt-5.2-codex");
    }
}
