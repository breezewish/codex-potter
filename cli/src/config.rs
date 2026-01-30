use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;

use toml_edit::DocumentMut;
use toml_edit::Item as TomlItem;
use toml_edit::Table as TomlTable;
use toml_edit::value;

use crate::atomic_write::write_atomic_text;

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn new_default() -> anyhow::Result<Self> {
        let Some(home) = dirs::home_dir() else {
            anyhow::bail!("cannot determine home directory for config path");
        };
        let xdg_config_home = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        Ok(Self::new(default_config_path(
            &home,
            xdg_config_home.as_deref(),
        )))
    }

    pub fn notice_hide_gitignore_prompt(&self) -> anyhow::Result<bool> {
        let Some(content) = read_document_string(&self.path)? else {
            return Ok(false);
        };

        let doc = match content.parse::<DocumentMut>() {
            Ok(doc) => doc,
            Err(_) => {
                return Ok(parse_notice_hide_gitignore_prompt_fallback(&content).unwrap_or(false));
            }
        };

        Ok(read_notice_hide_gitignore_prompt(&doc).unwrap_or(false))
    }

    pub fn set_notice_hide_gitignore_prompt(&self, hide: bool) -> anyhow::Result<()> {
        let content = match read_document_string(&self.path) {
            Ok(Some(existing)) => existing,
            Ok(None) => String::new(),
            Err(err) => {
                // If we can't read the existing file, avoid clobbering it; but still allow the
                // application to proceed.
                return Err(err);
            }
        };

        let updated = match content.parse::<DocumentMut>() {
            Ok(mut doc) => {
                set_notice_hide_gitignore_prompt(&mut doc, hide);
                doc.to_string()
            }
            Err(_) => append_notice_fallback(&content, hide),
        };

        write_atomic_text(&self.path, &updated)
    }
}

fn default_config_path(home: &Path, xdg_config_home: Option<&Path>) -> PathBuf {
    let base = xdg_config_home
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    base.join("codexpotter").join("config.toml")
}

fn read_notice_hide_gitignore_prompt(doc: &DocumentMut) -> Option<bool> {
    doc.get("notice")
        .and_then(TomlItem::as_table)
        .and_then(|notice| notice.get("hide_gitignore_prompt"))
        .and_then(TomlItem::as_value)
        .and_then(|v| v.as_bool())
}

fn set_notice_hide_gitignore_prompt(doc: &mut DocumentMut, hide: bool) {
    let notice = ensure_table_for_write(doc, "notice");
    notice["hide_gitignore_prompt"] = value(hide);
}

fn parse_notice_hide_gitignore_prompt_fallback(contents: &str) -> Option<bool> {
    let mut in_notice = false;
    let mut result = None;

    for line in contents.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            in_notice = matches!(parse_table_header_name(trimmed), Some("notice"));
            continue;
        }

        if !in_notice {
            continue;
        }

        let Some(line) = strip_toml_comment(trimmed) else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "hide_gitignore_prompt" {
            continue;
        }

        let token = value.split_whitespace().next().unwrap_or_default();
        if token == "true" {
            result = Some(true);
        } else if token == "false" {
            result = Some(false);
        }
    }

    result
}

fn parse_table_header_name(line: &str) -> Option<&str> {
    let line = line.trim_start();
    if !line.starts_with('[') {
        return None;
    }
    let end = line.find(']')?;
    if end <= 1 {
        return None;
    }
    let name = line[1..end].trim();
    if name.is_empty() {
        return None;
    }
    Some(name)
}

fn strip_toml_comment(line: &str) -> Option<&str> {
    let line = line.split_once('#').map_or(line, |(head, _)| head).trim();
    if line.is_empty() { None } else { Some(line) }
}

fn ensure_table_for_write<'a>(doc: &'a mut DocumentMut, key: &str) -> &'a mut TomlTable {
    if doc.get(key).and_then(TomlItem::as_table).is_some() {
        match &mut doc[key] {
            TomlItem::Table(table) => return table,
            _ => unreachable!("expected `{key}` to be a table"),
        }
    }

    let mut table = TomlTable::new();
    table.set_implicit(false);
    doc[key] = TomlItem::Table(table);
    match &mut doc[key] {
        TomlItem::Table(table) => table,
        _ => unreachable!("expected inserted `{key}` to be a table"),
    }
}

fn append_notice_fallback(existing: &str, hide: bool) -> String {
    let mut out = existing.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str("[notice]\n");
    out.push_str(&format!("hide_gitignore_prompt = {hide}\n"));
    out
}

fn read_document_string(path: &Path) -> anyhow::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(anyhow::Error::new(err).context("read config.toml")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_comments_and_sets_notice_flag() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"# top comment

[notice] # keep me
# inner comment
hide_gitignore_prompt = false

[other]
key = 1
"#,
        )
        .expect("write config");

        let store = ConfigStore::new(path.clone());
        store
            .set_notice_hide_gitignore_prompt(true)
            .expect("set flag");

        let updated = std::fs::read_to_string(&path).expect("read updated");
        assert!(updated.contains("# top comment"));
        assert!(updated.contains("# inner comment"));
        assert!(updated.contains("[other]"));
        assert!(updated.contains("hide_gitignore_prompt = true"));
        assert!(store.notice_hide_gitignore_prompt().expect("read flag"));
    }

    #[test]
    fn reads_notice_flag_when_toml_is_invalid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"# broken table header makes this TOML invalid
[other
key = 1

[notice]
hide_gitignore_prompt = true # keep me
"#,
        )
        .expect("write config");

        let store = ConfigStore::new(path);
        assert!(store.notice_hide_gitignore_prompt().expect("read flag"));
    }

    #[test]
    fn default_config_path_prefers_xdg_config_home_when_set() {
        let home = Path::new("home");
        let xdg = Path::new("xdg");

        assert_eq!(
            default_config_path(home, None),
            home.join(".config").join("codexpotter").join("config.toml")
        );
        assert_eq!(
            default_config_path(home, Some(xdg)),
            xdg.join("codexpotter").join("config.toml")
        );
    }
}
