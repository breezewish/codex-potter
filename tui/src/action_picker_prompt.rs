use crossterm::event::KeyCode;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::prelude::Widget;
use ratatui::text::Line;
use tokio_stream::StreamExt;

use crate::bottom_pane::ListSelectionView;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::render::renderable::Renderable;
use crate::tui::Tui;
use crate::tui::TuiEvent;

pub async fn prompt_action_picker(
    tui: &mut Tui,
    actions: Vec<String>,
) -> anyhow::Result<Option<usize>> {
    let items: Vec<SelectionItem> = actions
        .into_iter()
        .map(|name| SelectionItem {
            name,
            dismiss_on_select: true,
            ..Default::default()
        })
        .collect();

    let mut view = ListSelectionView::new(SelectionViewParams {
        title: Some("Select Action".to_string()),
        footer_hint: Some(Line::from("Press enter to run, or esc to exit.")),
        items,
        ..Default::default()
    });

    let width = tui.terminal.last_known_screen_size.width.max(1);
    tui.draw(view.desired_height(width).saturating_add(1), |frame| {
        let area = frame.area();
        ratatui::widgets::Clear.render(area, frame.buffer_mut());
        let view_area = ratatui::layout::Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(1),
        );
        view.render(view_area, frame.buffer_mut());
    })?;

    let events = tui.event_stream();
    tokio::pin!(events);

    while !view.is_complete() {
        let Some(event) = events.next().await else {
            break;
        };
        match event {
            TuiEvent::Key(key_event) => {
                if key_event.kind == KeyEventKind::Release {
                    continue;
                }
                if key_event.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key_event.code, KeyCode::Char('c'))
                {
                    if key_event.kind == KeyEventKind::Press {
                        view.cancel();
                    }
                } else {
                    view.handle_key_event(key_event);
                }
                tui.frame_requester().schedule_frame();
            }
            TuiEvent::Paste(_) => {}
            TuiEvent::Draw => {
                let width = tui.terminal.last_known_screen_size.width.max(1);
                tui.draw(view.desired_height(width).saturating_add(1), |frame| {
                    let area = frame.area();
                    ratatui::widgets::Clear.render(area, frame.buffer_mut());
                    let view_area = ratatui::layout::Rect::new(
                        area.x,
                        area.y.saturating_add(1),
                        area.width,
                        area.height.saturating_sub(1),
                    );
                    view.render(view_area, frame.buffer_mut());
                })?;
            }
        }
    }

    // Clear the inline viewport so subsequent screens start clean.
    tui.terminal.clear()?;

    Ok(view.take_last_selected_index())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn action_picker_prompt_renders_with_top_padding_line() {
        let view = ListSelectionView::new(SelectionViewParams {
            title: Some("Select Action".to_string()),
            footer_hint: Some(Line::from("Press enter to run, or esc to exit.")),
            items: vec![SelectionItem {
                name: "Iterate 10 more rounds".to_string(),
                dismiss_on_select: true,
                ..Default::default()
            }],
            ..Default::default()
        });

        let width = 54;
        let height = view.desired_height(width).saturating_add(1);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| {
                let area = frame.area();
                ratatui::widgets::Clear.render(area, frame.buffer_mut());
                let view_area = ratatui::layout::Rect::new(
                    area.x,
                    area.y.saturating_add(1),
                    area.width,
                    area.height.saturating_sub(1),
                );
                view.render(view_area, frame.buffer_mut());
            })
            .expect("draw");

        insta::assert_snapshot!(terminal.backend());
    }
}
