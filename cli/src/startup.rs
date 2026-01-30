use std::path::Path;
use std::path::PathBuf;

use crate::path_utils;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexBinError {
    NotFoundInPath { command: String },
    InvalidPath { path: PathBuf, reason: String },
}

impl CodexBinError {
    pub fn render_ansi(&self) -> String {
        match self {
            CodexBinError::NotFoundInPath { command } => {
                let url = "https://developers.openai.com/codex/quickstart?setup=cli";
                ansi_red(format!(
                    "Failed to find `{command}` binary.\n\
                     codex-potter requires codex CLI installed locally.\n\
                     \n\
                     See {url}\n",
                    url = ansi_underline(url),
                ))
            }
            CodexBinError::InvalidPath { path, reason } => ansi_red(format!(
                "Failed to find codex binary specified by `--codex-bin`: {} ({reason}).\n",
                path.display()
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCodexBin {
    pub command_for_spawn: String,
}

pub fn resolve_codex_bin(codex_bin: &str) -> Result<ResolvedCodexBin, CodexBinError> {
    if looks_like_path(codex_bin) {
        let path = path_utils::expand_tilde(Path::new(codex_bin));
        validate_executable_path(&path)?;
        return Ok(ResolvedCodexBin {
            command_for_spawn: path.display().to_string(),
        });
    }

    let resolved = which::which(codex_bin).map_err(|_| CodexBinError::NotFoundInPath {
        command: codex_bin.to_string(),
    })?;

    Ok(ResolvedCodexBin {
        command_for_spawn: resolved.display().to_string(),
    })
}

fn validate_executable_path(path: &Path) -> Result<(), CodexBinError> {
    let meta = std::fs::metadata(path).map_err(|err| CodexBinError::InvalidPath {
        path: path.to_path_buf(),
        reason: describe_metadata_error(&err),
    })?;

    if !meta.is_file() {
        return Err(CodexBinError::InvalidPath {
            path: path.to_path_buf(),
            reason: "not a file".to_string(),
        });
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = meta.permissions().mode();
        if mode & 0o111 == 0 {
            return Err(CodexBinError::InvalidPath {
                path: path.to_path_buf(),
                reason: "not executable".to_string(),
            });
        }
    }

    Ok(())
}

fn describe_metadata_error(err: &std::io::Error) -> String {
    match err.kind() {
        std::io::ErrorKind::NotFound => "does not exist".to_string(),
        std::io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        _ => err.to_string(),
    }
}

fn looks_like_path(value: &str) -> bool {
    let path = Path::new(value);
    path.is_absolute()
        || value.contains('/')
        || value.contains('\\')
        || value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with("~/")
        || value == "~"
}

fn ansi_red(text: String) -> String {
    format!("\u{1b}[31m{text}\u{1b}[0m")
}

fn ansi_underline(text: &str) -> String {
    format!("\u{1b}[4m{text}\u{1b}[24m")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalize_newlines_for_vt100(input: &str) -> String {
        // Most terminals (and tty line disciplines) translate '\n' to '\r\n'. The vt100 parser
        // does not, so do it here to keep snapshots aligned with real output.
        let mut out = String::with_capacity(input.len());
        let mut prev_was_cr = false;
        for ch in input.chars() {
            if ch == '\n' {
                if !prev_was_cr {
                    out.push('\r');
                }
                out.push('\n');
                prev_was_cr = false;
            } else {
                prev_was_cr = ch == '\r';
                out.push(ch);
            }
        }
        out
    }

    fn ansi_to_vt100_contents(rendered: &str) -> String {
        let mut parser = vt100::Parser::new(10, 120, 0);
        let normalized = normalize_newlines_for_vt100(rendered);
        parser.process(normalized.as_bytes());
        parser.screen().contents()
    }

    #[test]
    fn not_found_error_snapshot_includes_link_and_styles() {
        let err = CodexBinError::NotFoundInPath {
            command: "codex".to_string(),
        };
        let rendered = err.render_ansi();

        assert!(rendered.contains("https://developers.openai.com/codex/quickstart?setup=cli"));
        assert!(rendered.contains("\u{1b}[31m"), "should include red ANSI");
        assert!(rendered.contains("\u{1b}[4m"), "should underline link");

        insta::assert_snapshot!(ansi_to_vt100_contents(&rendered));
    }

    #[test]
    fn invalid_path_error_snapshot_has_no_quickstart_link() {
        let err = CodexBinError::InvalidPath {
            path: PathBuf::from("/nope/codex"),
            reason: "does not exist".to_string(),
        };
        let rendered = err.render_ansi();

        assert!(!rendered.contains("quickstart?setup=cli"));
        insta::assert_snapshot!(ansi_to_vt100_contents(&rendered));
    }

    #[test]
    fn validate_executable_path_maps_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("missing-codex-bin");

        let err = validate_executable_path(&path).expect_err("should fail");
        assert_eq!(
            err,
            CodexBinError::InvalidPath {
                path,
                reason: "does not exist".to_string()
            }
        );
    }
}
