use std::path::Path;
use std::path::PathBuf;

pub fn expand_tilde(path: &Path) -> PathBuf {
    let Some(path_str) = path.to_str() else {
        return path.to_path_buf();
    };
    if path_str == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(path_str));
    }
    let Some(rest) = path_str.strip_prefix("~/") else {
        return path.to_path_buf();
    };
    let Some(home) = dirs::home_dir() else {
        return path.to_path_buf();
    };
    home.join(rest)
}

pub fn display_with_tilde(path: &Path) -> String {
    let Some(home) = dirs::home_dir() else {
        return path.display().to_string();
    };

    let Ok(stripped) = path.strip_prefix(&home) else {
        return path.display().to_string();
    };

    if stripped.as_os_str().is_empty() {
        return "~".to_string();
    }

    format!("~/{}", stripped.display())
}
