use std::path::Path;
use std::path::PathBuf;

use dirs::home_dir;
use shlex::try_join;

pub fn extract_bash_command(command: &[String]) -> Option<(&str, &str)> {
    let [shell, flag, script] = command else {
        return None;
    };

    if !matches!(flag.as_str(), "-lc" | "-c") {
        return None;
    }

    let shell_name = Path::new(shell).file_name()?.to_str()?;
    let shell_name = shell_name.to_ascii_lowercase();
    let shell_name = shell_name
        .strip_suffix(".exe")
        .unwrap_or(shell_name.as_str());

    if !matches!(shell_name, "bash" | "zsh" | "sh") {
        return None;
    }

    Some((shell, script))
}

fn extract_shell_command(command: &[String]) -> Option<(&str, &str)> {
    extract_bash_command(command).or_else(|| extract_powershell_command(command))
}

const POWERSHELL_FLAGS: &[&str] = &["-nologo", "-noprofile", "-command", "-c"];

fn extract_powershell_command(command: &[String]) -> Option<(&str, &str)> {
    if command.len() < 3 {
        return None;
    }

    let shell = &command[0];
    let shell_name = Path::new(shell).file_name()?.to_str()?;
    let shell_name = shell_name.to_ascii_lowercase();
    let shell_name = shell_name
        .strip_suffix(".exe")
        .unwrap_or(shell_name.as_str());

    if !matches!(shell_name, "pwsh" | "powershell") {
        return None;
    }

    let mut i = 1usize;
    while i + 1 < command.len() {
        let flag = &command[i];
        if !POWERSHELL_FLAGS.contains(&flag.to_ascii_lowercase().as_str()) {
            return None;
        }
        if flag.eq_ignore_ascii_case("-Command") || flag.eq_ignore_ascii_case("-c") {
            return Some((shell, command[i + 1].as_str()));
        }
        i += 1;
    }

    None
}

pub fn escape_command(command: &[String]) -> String {
    try_join(command.iter().map(String::as_str)).unwrap_or_else(|_| command.join(" "))
}

pub fn strip_bash_lc_and_escape(command: &[String]) -> String {
    if let Some((_, script)) = extract_shell_command(command) {
        return script.to_string();
    }
    escape_command(command)
}

/// If `path` is absolute and inside $HOME, return the part *after* the home
/// directory; otherwise, return the path as-is. Note if `path` is the homedir,
/// this will return and empty path.
pub fn relativize_to_home<P>(path: P) -> Option<PathBuf>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();
    if !path.is_absolute() {
        // If the path is not absolute, we can’t do anything with it.
        return None;
    }

    let home_dir = home_dir()?;
    let rel = path.strip_prefix(&home_dir).ok()?;
    Some(rel.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_command() {
        let args = vec!["foo".into(), "bar baz".into(), "weird&stuff".into()];
        let cmdline = escape_command(&args);
        assert_eq!(cmdline, "foo 'bar baz' 'weird&stuff'");
    }

    #[test]
    fn test_strip_bash_lc_and_escape() {
        // Test bash
        let args = vec!["bash".into(), "-lc".into(), "echo hello".into()];
        let cmdline = strip_bash_lc_and_escape(&args);
        assert_eq!(cmdline, "echo hello");

        // Test zsh
        let args = vec!["zsh".into(), "-lc".into(), "echo hello".into()];
        let cmdline = strip_bash_lc_and_escape(&args);
        assert_eq!(cmdline, "echo hello");

        // Test absolute path to zsh
        let args = vec!["/usr/bin/zsh".into(), "-lc".into(), "echo hello".into()];
        let cmdline = strip_bash_lc_and_escape(&args);
        assert_eq!(cmdline, "echo hello");

        // Test absolute path to bash
        let args = vec!["/bin/bash".into(), "-lc".into(), "echo hello".into()];
        let cmdline = strip_bash_lc_and_escape(&args);
        assert_eq!(cmdline, "echo hello");
    }

    #[test]
    fn strip_bash_lc_and_escape_supports_powershell() {
        let args = vec![
            "powershell".into(),
            "-NoLogo".into(),
            "-NoProfile".into(),
            "-Command".into(),
            "Write-Host hi".into(),
        ];
        let cmdline = strip_bash_lc_and_escape(&args);
        assert_eq!(cmdline, "Write-Host hi");
    }

    #[test]
    fn strip_bash_lc_and_escape_rejects_powershell_unknown_flags() {
        let args = vec![
            "powershell".into(),
            "-NoLogo".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-Command".into(),
            "Write-Host hi".into(),
        ];
        let cmdline = strip_bash_lc_and_escape(&args);
        assert_eq!(
            cmdline,
            "powershell -NoLogo -ExecutionPolicy Bypass -Command 'Write-Host hi'"
        );
    }
}
