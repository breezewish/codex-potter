use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;

use crate::bottom_pane::ChatComposer;
use crate::external_editor;
use crate::tui;
use crate::tui::Tui;

pub const EXTERNAL_EDITOR_HINT: &str = "Save and close external editor to continue.";
pub const MISSING_EDITOR_ERROR: &str = "Cannot open external editor: set $VISUAL or $EDITOR";

pub fn is_ctrl_g(key_event: &KeyEvent) -> bool {
    key_event.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key_event.code, KeyCode::Char('g'))
}

pub async fn run_external_editor(
    tui: &mut Tui,
    composer: &ChatComposer,
) -> anyhow::Result<Option<String>> {
    let editor_cmd = match external_editor::resolve_editor_command() {
        Ok(cmd) => cmd,
        Err(external_editor::EditorError::MissingEditor) => return Ok(None),
        Err(err) => return Err(anyhow::Error::new(err)),
    };

    let seed = composer.current_text_with_pending();
    let editor_result = tui
        .with_restored(tui::RestoreMode::KeepRaw, || async {
            external_editor::run_editor(&seed, &editor_cmd).await
        })
        .await;

    match editor_result {
        Ok(new_text) => Ok(Some(new_text.trim_end().to_string())),
        Err(err) => Err(anyhow::Error::msg(err.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_ctrl_g_matches_only_ctrl_g() {
        assert!(is_ctrl_g(&KeyEvent::new(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL
        )));

        assert!(!is_ctrl_g(&KeyEvent::new(
            KeyCode::Char('g'),
            KeyModifiers::NONE
        )));
        assert!(!is_ctrl_g(&KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_ctrl_g(&KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::CONTROL
        )));
    }
}
