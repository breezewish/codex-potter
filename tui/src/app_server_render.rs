use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TurnStartedEvent;
use codex_protocol::user_input::UserInput;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::text::Line;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::unbounded_channel;
use tokio_stream::StreamExt;

use crate::AppExitInfo;
use crate::ExitReason;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPane;
use crate::bottom_pane::BottomPaneParams;
use crate::bottom_pane::ChatComposer;
use crate::bottom_pane::InputResult;
use crate::bottom_pane::PromptFooterOverride;
use crate::exec_cell::CommandOutput;
use crate::exec_cell::ExecCell;
use crate::exec_cell::new_active_exec_command;
use crate::external_editor_integration;
use crate::file_search::FileSearchManager;
use crate::history_cell;
use crate::history_cell::HistoryCell;
use crate::render::renderable::Renderable;
use crate::streaming::controller::StreamController;
use crate::tui::Tui;
use crate::tui::TuiEvent;

fn render_render_only_viewport(
    area: ratatui::layout::Rect,
    buf: &mut ratatui::buffer::Buffer,
    bottom_pane: &BottomPane,
    transient_lines: Vec<Line<'static>>,
) {
    let width = area.width;
    let pane_height = bottom_pane.desired_height(width).max(1).min(area.height);
    let pane_area = ratatui::layout::Rect::new(
        area.x,
        area.bottom().saturating_sub(pane_height),
        area.width,
        pane_height,
    );

    let transient_area_height = area.height.saturating_sub(pane_height);
    if transient_area_height > 0 && !transient_lines.is_empty() {
        let transient_area =
            ratatui::layout::Rect::new(area.x, area.y, area.width, transient_area_height);
        let overflow = transient_lines
            .len()
            .saturating_sub(usize::from(transient_area_height));
        let scroll_y = u16::try_from(overflow).unwrap_or(u16::MAX);
        ratatui::widgets::Paragraph::new(ratatui::text::Text::from(transient_lines))
            .scroll((scroll_y, 0))
            .render(transient_area, buf);
    }

    bottom_pane.render(pane_area, buf);
}

const PROMPT_PLACEHOLDER_TEXT: &str = "Assign new task to CodexPotter";

fn new_default_bottom_pane(
    tui: &Tui,
    app_event_tx: AppEventSender,
    animations_enabled: bool,
) -> BottomPane {
    BottomPane::new(BottomPaneParams {
        frame_requester: tui.frame_requester(),
        enhanced_keys_supported: tui.enhanced_keys_supported(),
        app_event_tx,
        animations_enabled,
        placeholder_text: PROMPT_PLACEHOLDER_TEXT.to_string(),
        disable_paste_burst: false,
    })
}

pub async fn prompt_user_with_tui(tui: &mut Tui) -> anyhow::Result<Option<String>> {
    let (app_event_tx_raw, mut app_event_rx) = unbounded_channel::<AppEvent>();
    let app_event_tx = AppEventSender::new(app_event_tx_raw);

    let file_search_dir = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
    let file_search = FileSearchManager::new(file_search_dir.clone(), app_event_tx.clone());
    let mut prompt_history = crate::prompt_history_store::PromptHistoryStore::new();

    let width = tui.terminal.last_known_screen_size.width.max(1);
    let codex_model = crate::codex_config::resolve_codex_model_config(&file_search_dir)?;
    let model_label = match codex_model.reasoning_effort {
        Some(effort) => format!("{} {effort}", codex_model.model),
        None => codex_model.model,
    };
    tui.insert_history_lines(crate::startup_banner::build_startup_banner_lines(
        width,
        env!("CARGO_PKG_VERSION"),
        &model_label,
        &file_search_dir,
    ));

    let mut bottom_pane = new_default_bottom_pane(tui, app_event_tx.clone(), true);
    let (history_log_id, history_entry_count) = prompt_history.metadata();
    bottom_pane
        .composer_mut()
        .set_history_metadata(history_log_id, history_entry_count);

    let mut tui_events = tui.event_stream();
    tui.frame_requester().schedule_frame();

    loop {
        tokio::select! {
            maybe_event = tui_events.next() => {
                let Some(event) = maybe_event else {
                    return Ok(None);
                };
                match event {
                    TuiEvent::Draw => {
                        let width = tui.terminal.last_known_screen_size.width;
                        if bottom_pane.composer_mut().flush_paste_burst_if_due() {
                            // A paste just flushed; request an immediate redraw and skip this frame.
                            tui.frame_requester().schedule_frame();
                            continue;
                        }
                        if bottom_pane.composer().is_in_paste_burst() {
                            // While capturing a burst, schedule a follow-up tick and skip this frame
                            // to avoid redundant renders between ticks.
                            tui.frame_requester().schedule_frame_in(
                                ChatComposer::recommended_paste_flush_delay(),
                            );
                            continue;
                        }

                        while let Ok(app_event) = app_event_rx.try_recv() {
                            handle_prompt_app_event(
                                tui,
                                &mut bottom_pane,
                                &file_search,
                                &mut prompt_history,
                                app_event,
                            );
                        }

                        draw_prompt_bottom_pane(tui, &bottom_pane, width)?;
                    }
                    TuiEvent::Key(key_event) => {
                        if key_event.kind == crossterm::event::KeyEventKind::Release {
                            continue;
                        }

                        let is_press = key_event.kind == crossterm::event::KeyEventKind::Press;

                        if key_event
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL)
                            && matches!(key_event.code, crossterm::event::KeyCode::Char('c'))
                        {
                            if !is_press {
                                continue;
                            }
                            if bottom_pane.composer().is_empty() {
                                // Clear the inline viewport so the shell prompt is clean on exit.
                                tui.terminal.clear()?;
                                return Ok(None);
                            }
                            bottom_pane.composer_mut().clear_for_ctrl_c();
                            tui.frame_requester().schedule_frame();
                            continue;
                        }

                        if external_editor_integration::is_ctrl_g(&key_event) {
                            if !is_press {
                                continue;
                            }
                            bottom_pane.set_prompt_footer_override(Some(PromptFooterOverride::ExternalEditorHint));
                            let width = tui.terminal.last_known_screen_size.width;
                            draw_prompt_bottom_pane(tui, &bottom_pane, width)?;
                            match external_editor_integration::run_external_editor(tui, bottom_pane.composer())
                                .await
                            {
                                Ok(Some(new_text)) => {
                                    bottom_pane.composer_mut().apply_external_edit(new_text);
                                    tui.frame_requester().schedule_frame();
                                }
                                Ok(None) => {
                                    handle_prompt_app_event(
                                        tui,
                                        &mut bottom_pane,
                                        &file_search,
                                        &mut prompt_history,
                                        AppEvent::InsertHistoryCell(Box::new(history_cell::new_error_event(
                                            external_editor_integration::MISSING_EDITOR_ERROR.to_string(),
                                        ))),
                                    );
                                }
                                Err(err) => {
                                    handle_prompt_app_event(
                                        tui,
                                        &mut bottom_pane,
                                        &file_search,
                                        &mut prompt_history,
                                        AppEvent::InsertHistoryCell(Box::new(history_cell::new_error_event(format!(
                                            "Failed to open editor: {err}",
                                        )))),
                                    );
                                }
                            }
                            bottom_pane.set_prompt_footer_override(None);
                            continue;
                        }

                        let (result, needs_redraw) =
                            bottom_pane.composer_mut().handle_key_event(key_event);
                        if needs_redraw {
                            tui.frame_requester().schedule_frame();
                        }
                        if bottom_pane.composer().is_in_paste_burst() {
                            tui.frame_requester().schedule_frame_in(ChatComposer::recommended_paste_flush_delay());
                        }
                        match result {
                            InputResult::Submitted(text) | InputResult::Queued(text) => {
                                prompt_history.record_submission(&text);
                                return Ok(Some(text));
                            }
                            _ => {}
                        }
                    }
                    TuiEvent::Paste(pasted) => {
                        // Many terminals convert newlines to \r when pasting (e.g., iTerm2),
                        // but tui-textarea expects \n. Normalize CR to LF.
                        let pasted = pasted.replace("\r", "\n");
                        if bottom_pane.composer_mut().handle_paste(pasted) {
                            tui.frame_requester().schedule_frame();
                        }
                    }
                }
            }
            maybe_app_event = app_event_rx.recv() => {
                let Some(app_event) = maybe_app_event else {
                    return Ok(None);
                };
                handle_prompt_app_event(tui, &mut bottom_pane, &file_search, &mut prompt_history, app_event);
            }
        }
    }
}

fn handle_prompt_app_event(
    tui: &mut Tui,
    bottom_pane: &mut BottomPane,
    file_search: &FileSearchManager,
    prompt_history: &mut crate::prompt_history_store::PromptHistoryStore,
    app_event: AppEvent,
) {
    match app_event {
        AppEvent::StartFileSearch(query) => file_search.on_user_query(query),
        AppEvent::FileSearchResult { query, matches } => {
            bottom_pane
                .composer_mut()
                .on_file_search_result(query, matches);
            tui.frame_requester().schedule_frame();
        }
        AppEvent::InsertHistoryCell(cell) => {
            let width = tui.terminal.last_known_screen_size.width;
            tui.insert_history_lines(cell.display_lines(width));
        }
        AppEvent::CodexOp(Op::GetHistoryEntryRequest { offset, log_id }) => {
            let entry = prompt_history.lookup_text(log_id, offset);
            if bottom_pane
                .composer_mut()
                .on_history_entry_response(log_id, offset, entry)
            {
                tui.frame_requester().schedule_frame();
            }
        }
        _ => {}
    }
}

fn draw_prompt_bottom_pane(
    tui: &mut Tui,
    bottom_pane: &BottomPane,
    width: u16,
) -> anyhow::Result<()> {
    let viewport_height = bottom_pane.desired_height(width).max(1);
    tui.draw(viewport_height, |frame| {
        let area = frame.area();
        ratatui::widgets::Clear.render(area, frame.buffer_mut());
        render_render_only_viewport(area, frame.buffer_mut(), bottom_pane, Vec::new());

        let pane_height = bottom_pane
            .desired_height(area.width)
            .max(1)
            .min(area.height);
        let pane_area = Rect::new(
            area.x,
            area.bottom().saturating_sub(pane_height),
            area.width,
            pane_height,
        );
        let cursor = bottom_pane
            .cursor_pos(pane_area)
            .unwrap_or((area.x, area.bottom().saturating_sub(1)));
        frame.set_cursor_position(cursor);
    })?;

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOnlyTurnOptions {
    /// When true, renders the user prompt into the transcript before sending it to the backend.
    pub render_user_prompt: bool,

    /// When true, inserts a blank line before the first emitted history cell in this single-turn
    /// session. This is useful when multiple turns are rendered into the same terminal transcript
    /// (multi-round runners) while suppressing the per-turn user prompt rendering.
    pub pad_before_first_cell: bool,
}

impl Default for RenderOnlyTurnOptions {
    fn default() -> Self {
        Self {
            render_user_prompt: true,
            pad_before_first_cell: false,
        }
    }
}

pub struct RenderOnlyBackendChannels {
    pub codex_op_tx: UnboundedSender<Op>,
    pub codex_event_rx: UnboundedReceiver<Event>,
    pub fatal_exit_rx: UnboundedReceiver<String>,
}

pub async fn run_render_only_with_tui_options_and_queue(
    tui: &mut Tui,
    prompt: String,
    options: RenderOnlyTurnOptions,
    backend: RenderOnlyBackendChannels,
    queued_user_messages: &mut VecDeque<String>,
    composer_draft: &mut Option<crate::bottom_pane::ChatComposerDraft>,
) -> anyhow::Result<AppExitInfo> {
    let RenderOnlyBackendChannels {
        codex_op_tx,
        mut codex_event_rx,
        mut fatal_exit_rx,
    } = backend;

    let (app_event_tx_raw, mut app_event_rx) = unbounded_channel::<AppEvent>();
    let app_event_tx = AppEventSender::new(app_event_tx_raw);

    let file_search_dir = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
    let file_search = FileSearchManager::new(file_search_dir, app_event_tx.clone());
    let prompt_history = crate::prompt_history_store::PromptHistoryStore::new();

    let driver = RenderOnlyProcessor::new(app_event_tx.clone());
    if options.render_user_prompt {
        driver.emit_user_prompt(prompt.clone());
    }

    codex_op_tx
        .send(Op::UserInput {
            items: vec![UserInput::Text {
                text: prompt,
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .map_err(|err| anyhow::Error::msg(err.to_string()))?;

    let mut bottom_pane = new_default_bottom_pane(tui, app_event_tx.clone(), true);
    if let Some(draft) = composer_draft.take() {
        bottom_pane.composer_mut().restore_draft(draft);
    }
    let (history_log_id, history_entry_count) = prompt_history.metadata();
    bottom_pane
        .composer_mut()
        .set_history_metadata(history_log_id, history_entry_count);
    let queued_user_messages_state = std::mem::take(queued_user_messages);
    let mut app = RenderAppState::new(
        driver,
        app_event_tx.clone(),
        codex_op_tx,
        bottom_pane,
        prompt_history,
        file_search,
        queued_user_messages_state,
    );
    app.has_emitted_history_lines = options.pad_before_first_cell;
    app.refresh_queued_user_messages();

    let result = app
        .run(
            tui,
            &mut app_event_rx,
            &mut codex_event_rx,
            &mut fatal_exit_rx,
        )
        .await;
    *queued_user_messages = app.queued_user_messages;
    *composer_draft = app.bottom_pane.composer_mut().take_draft();
    result
}

struct RenderOnlyProcessor {
    app_event_tx: AppEventSender,
    stream: StreamController,
    token_usage: TokenUsage,
    context_usage: TokenUsage,
    model_context_window: Option<i64>,
    thread_id: Option<codex_protocol::ThreadId>,
    cwd: PathBuf,
    saw_agent_delta: bool,
    pending_exploring_cell: Option<ExecCell>,
    pending_success_ran_cell: Option<ExecCell>,
}

impl RenderOnlyProcessor {
    fn new(app_event_tx: AppEventSender) -> Self {
        Self {
            app_event_tx,
            stream: StreamController::new(None),
            token_usage: TokenUsage::default(),
            context_usage: TokenUsage::default(),
            model_context_window: None,
            thread_id: None,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            saw_agent_delta: false,
            pending_exploring_cell: None,
            pending_success_ran_cell: None,
        }
    }

    fn emit_user_prompt(&self, prompt: String) {
        self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
            history_cell::new_user_prompt(prompt),
        )));
    }

    fn on_commit_tick(&mut self) {
        let (cell, idle) = self.stream.on_commit_tick();
        if let Some(cell) = cell {
            self.app_event_tx.send(AppEvent::InsertHistoryCell(cell));
        }
        if idle {
            self.app_event_tx.send(AppEvent::StopCommitAnimation);
        }
    }

    fn handle_codex_event(&mut self, event: Event) {
        match event.msg {
            EventMsg::SessionConfigured(cfg) => {
                self.thread_id = Some(cfg.session_id);
                self.cwd = cfg.cwd;
            }
            EventMsg::PotterSessionStarted {
                user_message,
                user_prompt_file,
                ..
            } => {
                self.flush_pending_exploring_cell();
                self.flush_pending_success_ran_cell();
                if let Some(message) = user_message.filter(|message| !message.is_empty()) {
                    self.emit_user_prompt(message);
                }
                self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    crate::history_cell_potter::new_potter_project_hint(user_prompt_file),
                )));
            }
            EventMsg::PotterRoundStarted { current, total } => {
                self.flush_pending_exploring_cell();
                self.flush_pending_success_ran_cell();
                self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    crate::history_cell_potter::new_potter_round_started(current, total),
                )));
            }
            EventMsg::TokenCount(ev) => {
                if let Some(info) = ev.info {
                    self.token_usage = info.total_token_usage;
                    self.context_usage = info.last_token_usage;
                    self.model_context_window =
                        info.model_context_window.or(self.model_context_window);
                }
            }
            EventMsg::TurnStarted(TurnStartedEvent {
                model_context_window,
            }) => {
                self.model_context_window = model_context_window;
            }
            EventMsg::AgentMessageDelta(ev) => {
                self.flush_pending_exploring_cell();
                self.flush_pending_success_ran_cell();
                self.saw_agent_delta = true;
                if self.stream.push(&ev.delta) {
                    self.app_event_tx.send(AppEvent::StartCommitAnimation);
                }
            }
            EventMsg::AgentMessage(ev) => {
                self.flush_pending_exploring_cell();
                self.flush_pending_success_ran_cell();
                if self.saw_agent_delta {
                    return;
                }
                self.emit_agent_message(&ev.message);
            }
            EventMsg::TurnComplete(ev) => {
                self.flush_pending_exploring_cell();
                self.flush_pending_success_ran_cell();
                // Flush any remaining agent markdown buffer.
                if let Some(cell) = self.stream.finalize() {
                    self.app_event_tx.send(AppEvent::InsertHistoryCell(cell));
                } else if !self.saw_agent_delta
                    && let Some(last) = ev.last_agent_message
                {
                    self.emit_agent_message(&last);
                }
                self.app_event_tx.send(AppEvent::StopCommitAnimation);
            }
            EventMsg::TurnAborted(_) => {
                self.flush_pending_exploring_cell();
                self.flush_pending_success_ran_cell();
                if let Some(cell) = self.stream.finalize() {
                    self.app_event_tx.send(AppEvent::InsertHistoryCell(cell));
                }
                self.app_event_tx.send(AppEvent::StopCommitAnimation);
            }
            EventMsg::Warning(ev) => {
                self.flush_pending_exploring_cell();
                self.flush_pending_success_ran_cell();
                self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    history_cell::new_warning_event(ev.message),
                )));
            }
            EventMsg::ContextCompacted(_) => {
                self.flush_pending_exploring_cell();
                self.flush_pending_success_ran_cell();
                self.emit_agent_message("Context compacted");
            }
            EventMsg::DeprecationNotice(ev) => {
                self.flush_pending_exploring_cell();
                self.flush_pending_success_ran_cell();
                self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    history_cell::new_deprecation_notice(ev.summary, ev.details),
                )));
            }
            EventMsg::PlanUpdate(ev) => {
                self.flush_pending_exploring_cell();
                self.flush_pending_success_ran_cell();
                self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    history_cell::new_plan_update(ev),
                )));
            }
            EventMsg::WebSearchEnd(ev) => {
                self.flush_pending_exploring_cell();
                self.flush_pending_success_ran_cell();
                self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    history_cell::new_web_search_call(ev.query),
                )));
            }
            EventMsg::ViewImageToolCall(ev) => {
                self.flush_pending_exploring_cell();
                self.flush_pending_success_ran_cell();
                self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    history_cell::new_view_image_tool_call(ev.path, &self.cwd),
                )));
            }
            EventMsg::ExecCommandEnd(ev) => {
                let aggregated_output = if !ev.aggregated_output.is_empty() {
                    ev.aggregated_output
                } else {
                    format!("{}{}", ev.stdout, ev.stderr)
                };

                let mut cell = new_active_exec_command(
                    ev.call_id.clone(),
                    ev.command,
                    ev.parsed_cmd,
                    ev.source,
                    ev.interaction_input,
                    false,
                );
                cell.complete_call(
                    &ev.call_id,
                    CommandOutput {
                        exit_code: ev.exit_code,
                        aggregated_output,
                        formatted_output: ev.formatted_output,
                    },
                    ev.duration,
                );

                if cell.is_exploring_cell() {
                    self.flush_pending_success_ran_cell();
                    if let Some(pending) = self.pending_exploring_cell.as_mut() {
                        pending.calls.extend(cell.calls);
                    } else {
                        self.pending_exploring_cell = Some(cell);
                    }
                } else if Self::can_coalesce_success_ran_cell(&cell) {
                    self.flush_pending_exploring_cell();
                    if let Some(pending) = self.pending_success_ran_cell.as_mut() {
                        pending.calls.extend(cell.calls);
                    } else {
                        self.pending_success_ran_cell = Some(cell);
                    }
                } else {
                    self.flush_pending_exploring_cell();
                    self.flush_pending_success_ran_cell();
                    self.app_event_tx
                        .send(AppEvent::InsertHistoryCell(Box::new(cell)));
                }
            }
            EventMsg::PatchApplyEnd(ev) => {
                self.flush_pending_exploring_cell();
                self.flush_pending_success_ran_cell();
                if ev.success {
                    self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_patch_event(ev.changes, &self.cwd),
                    )));
                } else {
                    self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_patch_apply_failure(ev.stderr),
                    )));
                }
            }
            EventMsg::Error(ev) => {
                self.flush_pending_exploring_cell();
                self.flush_pending_success_ran_cell();
                self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    history_cell::new_error_event(ev.message),
                )));
            }
            _ => {}
        }
    }

    fn flush_pending_exploring_cell(&mut self) {
        let Some(cell) = self.pending_exploring_cell.take() else {
            return;
        };
        self.app_event_tx
            .send(AppEvent::InsertHistoryCell(Box::new(cell)));
    }

    fn flush_pending_success_ran_cell(&mut self) {
        let Some(cell) = self.pending_success_ran_cell.take() else {
            return;
        };
        self.app_event_tx
            .send(AppEvent::InsertHistoryCell(Box::new(cell)));
    }

    fn can_coalesce_success_ran_cell(cell: &ExecCell) -> bool {
        let [call] = cell.calls.as_slice() else {
            return false;
        };

        call.output
            .as_ref()
            .is_some_and(|output| output.exit_code == 0)
            && !call.is_user_shell_command()
            && !call.is_unified_exec_interaction()
    }

    fn emit_agent_message(&self, message: &str) {
        let mut lines: Vec<Line<'static>> = Vec::new();
        crate::markdown::append_markdown(message, None, &mut lines);
        if lines.is_empty() {
            return;
        }
        self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
            history_cell::AgentMessageCell::new(lines, true),
        )));
    }
}

struct ReasoningStatusTracker {
    buffer: String,
}

impl ReasoningStatusTracker {
    fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }

    fn reset(&mut self) {
        self.buffer.clear();
    }

    fn on_section_break(&mut self) {
        self.reset();
    }

    fn on_final(&mut self) {
        self.reset();
    }

    fn on_delta(&mut self, delta: &str) -> Option<String> {
        self.buffer.push_str(delta);
        extract_first_bold(&self.buffer)
    }
}

struct RenderAppState {
    processor: RenderOnlyProcessor,
    app_event_tx: AppEventSender,
    codex_op_tx: UnboundedSender<Op>,
    bottom_pane: BottomPane,
    prompt_history: crate::prompt_history_store::PromptHistoryStore,
    file_search: FileSearchManager,
    queued_user_messages: VecDeque<String>,
    reasoning_status: ReasoningStatusTracker,
    commit_anim_running: Arc<AtomicBool>,
    has_emitted_history_lines: bool,
    exit_after_next_draw: bool,
    exit_reason: ExitReason,
}

impl RenderAppState {
    fn new(
        processor: RenderOnlyProcessor,
        app_event_tx: AppEventSender,
        codex_op_tx: UnboundedSender<Op>,
        bottom_pane: BottomPane,
        prompt_history: crate::prompt_history_store::PromptHistoryStore,
        file_search: FileSearchManager,
        queued_user_messages: VecDeque<String>,
    ) -> Self {
        Self {
            processor,
            app_event_tx,
            codex_op_tx,
            bottom_pane,
            prompt_history,
            file_search,
            queued_user_messages,
            reasoning_status: ReasoningStatusTracker::new(),
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            has_emitted_history_lines: false,
            exit_after_next_draw: false,
            exit_reason: ExitReason::UserRequested,
        }
    }

    async fn run(
        &mut self,
        tui: &mut Tui,
        app_event_rx: &mut UnboundedReceiver<AppEvent>,
        codex_event_rx: &mut UnboundedReceiver<Event>,
        fatal_exit_rx: &mut UnboundedReceiver<String>,
    ) -> anyhow::Result<AppExitInfo> {
        let mut tui_events = tui.event_stream();
        self.bottom_pane.set_task_running(true);
        tui.frame_requester().schedule_frame();

        loop {
            tokio::select! {
                maybe_event = tui_events.next() => {
                    let Some(event) = maybe_event else {
                        break;
                    };
                        match event {
                        TuiEvent::Draw => {
                            if self.bottom_pane.composer_mut().flush_paste_burst_if_due() {
                                // A paste just flushed; request an immediate redraw and skip this frame.
                                tui.frame_requester().schedule_frame();
                                continue;
                            }
                            if self.bottom_pane.composer().is_in_paste_burst() {
                                // While capturing a burst, schedule a follow-up tick and skip this frame
                                // to avoid redundant renders.
                                tui.frame_requester().schedule_frame_in(crate::bottom_pane::ChatComposer::recommended_paste_flush_delay());
                                continue;
                            }
                            // Drain any queued events before drawing so the rendered frame reflects
                            // the latest history inserts. This also avoids a race where a scheduled
                            // Draw event wins the select! before the final InsertHistoryCell events
                            // are processed, which would otherwise cause this runner to exit with
                            // missing output.
                            while let Ok(app_event) = app_event_rx.try_recv() {
                                self.handle_app_event(tui, app_event)?;
                            }
                            while let Ok(event) = codex_event_rx.try_recv() {
                                self.handle_app_event(tui, AppEvent::CodexEvent(event))?;
                            }
                            while let Ok(message) = fatal_exit_rx.try_recv() {
                                self.handle_app_event(tui, AppEvent::FatalExitRequest(message))?;
                            }

                            // Drain any new app events produced by the codex events we just
                            // processed above before rendering the next frame.
                            while let Ok(app_event) = app_event_rx.try_recv() {
                                self.handle_app_event(tui, app_event)?;
                            }
                            self.draw(tui)?;
                            if self.exit_after_next_draw {
                                break;
                            }
                        }
                            TuiEvent::Key(key_event) => {
                                if external_editor_integration::is_ctrl_g(&key_event) {
                                    if key_event.kind == crossterm::event::KeyEventKind::Press {
                                        self.handle_external_editor(tui).await?;
                                    }
                                    continue;
                                }
                                self.handle_key_event(key_event, tui.frame_requester());
                            }
                        TuiEvent::Paste(pasted) => {
                            // Many terminals convert newlines to \r when pasting (e.g., iTerm2),
                            // but tui-textarea expects \n. Normalize CR to LF.
                            let pasted = pasted.replace("\r", "\n");
                            if self.bottom_pane.composer_mut().handle_paste(pasted) {
                                tui.frame_requester().schedule_frame();
                            }
                        }
                    }
                }
                maybe_app_event = app_event_rx.recv() => {
                    let Some(app_event) = maybe_app_event else {
                        break;
                    };
                    self.handle_app_event(tui, app_event)?;
                }
                maybe_codex_event = codex_event_rx.recv() => {
                    match maybe_codex_event {
                        Some(event) => {
                            self.handle_app_event(tui, AppEvent::CodexEvent(event))?;
                        }
                        None => {
                            if !self.exit_after_next_draw {
                                self.exit_reason = ExitReason::Fatal("Backend disconnected".to_string());
                                self.exit_after_next_draw = true;
                                tui.frame_requester().schedule_frame();
                            }
                        }
                    }
                }
                maybe_fatal = fatal_exit_rx.recv() => {
                    let Some(message) = maybe_fatal else {
                        continue;
                    };
                    self.handle_app_event(tui, AppEvent::FatalExitRequest(message))?;
                }
            }
        }

        self.commit_anim_running.store(false, Ordering::Release);
        Ok(AppExitInfo {
            token_usage: self.processor.token_usage.clone(),
            thread_id: self.processor.thread_id,
            exit_reason: self.exit_reason.clone(),
        })
    }

    async fn handle_external_editor(&mut self, tui: &mut Tui) -> anyhow::Result<()> {
        self.bottom_pane
            .set_prompt_footer_override(Some(PromptFooterOverride::ExternalEditorHint));
        self.draw(tui)?;

        match external_editor_integration::run_external_editor(tui, self.bottom_pane.composer())
            .await
        {
            Ok(Some(new_text)) => {
                self.bottom_pane
                    .composer_mut()
                    .apply_external_edit(new_text);
            }
            Ok(None) => {
                self.handle_app_event(
                    tui,
                    AppEvent::InsertHistoryCell(Box::new(history_cell::new_error_event(
                        external_editor_integration::MISSING_EDITOR_ERROR.to_string(),
                    ))),
                )?;
            }
            Err(err) => {
                self.handle_app_event(
                    tui,
                    AppEvent::InsertHistoryCell(Box::new(history_cell::new_error_event(format!(
                        "Failed to open editor: {err}",
                    )))),
                )?;
            }
        }

        self.bottom_pane.set_prompt_footer_override(None);
        tui.frame_requester().schedule_frame();
        Ok(())
    }

    fn handle_key_event(
        &mut self,
        key_event: crossterm::event::KeyEvent,
        frame_requester: crate::tui::FrameRequester,
    ) {
        if key_event.kind == crossterm::event::KeyEventKind::Release {
            return;
        }

        let is_press = key_event.kind == crossterm::event::KeyEventKind::Press;

        // Restore the last queued message into the composer for quick edits.
        if key_event.modifiers == crossterm::event::KeyModifiers::ALT
            && matches!(key_event.code, crossterm::event::KeyCode::Up)
            && !self.queued_user_messages.is_empty()
        {
            if !is_press {
                return;
            }
            if let Some(message) = self.queued_user_messages.pop_back() {
                self.bottom_pane.composer_mut().set_text_content(message);
                self.refresh_queued_user_messages();
                frame_requester.schedule_frame();
            }
            return;
        }

        if key_event
            .modifiers
            .contains(crossterm::event::KeyModifiers::CONTROL)
            && matches!(key_event.code, crossterm::event::KeyCode::Char('c'))
        {
            if !is_press {
                return;
            }
            if self.bottom_pane.composer().is_empty() {
                // Preserve any live output (for example pending "Explored" / "Ran" cells) in the
                // transcript before clearing the inline viewport on exit.
                self.processor.flush_pending_exploring_cell();
                self.processor.flush_pending_success_ran_cell();

                self.app_event_tx.send(AppEvent::CodexOp(Op::Interrupt));

                // Treat Ctrl+C as an explicit user cancellation, even if the turn just finished,
                // so callers can stop multi-round loops reliably.
                if !matches!(self.exit_reason, ExitReason::Fatal(_)) {
                    self.exit_reason = ExitReason::UserRequested;
                }
                self.exit_after_next_draw = true;
            } else {
                self.bottom_pane.composer_mut().clear_for_ctrl_c();
            }
            frame_requester.schedule_frame();
            return;
        }

        let (result, needs_redraw) = self.bottom_pane.composer_mut().handle_key_event(key_event);
        if needs_redraw {
            frame_requester.schedule_frame();
        }
        if self.bottom_pane.composer().is_in_paste_burst() {
            frame_requester.schedule_frame_in(
                crate::bottom_pane::ChatComposer::recommended_paste_flush_delay(),
            );
        }

        match result {
            InputResult::Submitted(text) | InputResult::Queued(text) => {
                self.prompt_history.record_submission(&text);
                self.queued_user_messages.push_back(text);
                self.refresh_queued_user_messages();
                frame_requester.schedule_frame();
            }
            InputResult::None => {}
        }
    }

    fn refresh_queued_user_messages(&mut self) {
        let messages: Vec<String> = self.queued_user_messages.iter().cloned().collect();
        self.bottom_pane.set_queued_user_messages(messages);
    }

    fn draw(&mut self, tui: &mut Tui) -> anyhow::Result<()> {
        let width = tui.terminal.last_known_screen_size.width;
        let pane_height = self.bottom_pane.desired_height(width).max(1);
        let mut transient_lines: Vec<Line<'static>> = Vec::new();
        if let Some(cell) = self.processor.pending_success_ran_cell.as_ref() {
            transient_lines.push(Line::from(""));
            transient_lines.extend(cell.display_lines(width));
        }
        if let Some(cell) = self.processor.pending_exploring_cell.as_ref() {
            // Keep a blank line between the transcript (which may include a background-colored
            // user prompt cell) and the live explored block.
            transient_lines.push(Line::from(""));
            transient_lines.extend(cell.display_lines(width));
        }
        let transient_height = u16::try_from(transient_lines.len()).unwrap_or(u16::MAX);
        let viewport_height = pane_height.saturating_add(transient_height);

        tui.draw(viewport_height, |frame| {
            let area = frame.area();
            ratatui::widgets::Clear.render(area, frame.buffer_mut());
            render_render_only_viewport(
                area,
                frame.buffer_mut(),
                &self.bottom_pane,
                transient_lines,
            );

            let pane_height = self
                .bottom_pane
                .desired_height(area.width)
                .max(1)
                .min(area.height);
            let pane_area = ratatui::layout::Rect::new(
                area.x,
                area.bottom().saturating_sub(pane_height),
                area.width,
                pane_height,
            );
            let cursor = self
                .bottom_pane
                .cursor_pos(pane_area)
                .unwrap_or((area.x, area.bottom().saturating_sub(1)));
            frame.set_cursor_position(cursor);
        })?;
        Ok(())
    }

    fn handle_app_event(&mut self, tui: &mut Tui, app_event: AppEvent) -> anyhow::Result<()> {
        match app_event {
            AppEvent::InsertHistoryCell(cell) => {
                let cell: Arc<dyn HistoryCell> = cell.into();
                let width = tui.terminal.last_known_screen_size.width;
                let mut display = cell.display_lines(width);
                if display.is_empty() {
                    return Ok(());
                }

                if !cell.is_stream_continuation() {
                    if self.has_emitted_history_lines {
                        display.insert(0, Line::from(""));
                    } else {
                        self.has_emitted_history_lines = true;
                    }
                }

                tui.insert_history_lines(display);
            }
            AppEvent::StartCommitAnimation => {
                if self
                    .commit_anim_running
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    let tx = self.app_event_tx.clone();
                    let running = self.commit_anim_running.clone();
                    thread::spawn(move || {
                        while running.load(Ordering::Relaxed) {
                            thread::sleep(Duration::from_millis(50));
                            tx.send(AppEvent::CommitTick);
                        }
                    });
                }
            }
            AppEvent::StopCommitAnimation => {
                self.commit_anim_running.store(false, Ordering::Release);
            }
            AppEvent::CommitTick => {
                self.processor.on_commit_tick();
            }
            AppEvent::CodexEvent(event) => {
                match &event.msg {
                    EventMsg::PotterRoundStarted { current, total } => {
                        self.bottom_pane
                            .set_status_header_prefix(Some(format!("Round {current}/{total}")));
                    }
                    EventMsg::TurnStarted(_) => {
                        self.reasoning_status.reset();
                        self.bottom_pane
                            .update_status_header(String::from("Working"));
                    }
                    EventMsg::AgentReasoningDelta(ev) => {
                        if let Some(header) = self.reasoning_status.on_delta(&ev.delta) {
                            self.bottom_pane.update_status_header(header);
                        }
                        return Ok(());
                    }
                    EventMsg::AgentReasoningRawContentDelta(ev) => {
                        if let Some(header) = self.reasoning_status.on_delta(&ev.delta) {
                            self.bottom_pane.update_status_header(header);
                        }
                        return Ok(());
                    }
                    EventMsg::AgentReasoningSectionBreak(_) => {
                        self.reasoning_status.on_section_break();
                        return Ok(());
                    }
                    EventMsg::AgentReasoning(_) => {
                        self.reasoning_status.on_final();
                        return Ok(());
                    }
                    EventMsg::AgentReasoningRawContent(ev) => {
                        if let Some(header) = self.reasoning_status.on_delta(&ev.text) {
                            self.bottom_pane.update_status_header(header);
                        }
                        self.reasoning_status.on_final();
                        return Ok(());
                    }
                    _ => {}
                }

                if should_filter_thinking_event(&event.msg) {
                    return Ok(());
                }

                let should_stop_footer = matches!(
                    event.msg,
                    EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_) | EventMsg::Error(_)
                );
                let should_update_context = matches!(
                    event.msg,
                    EventMsg::TokenCount(_) | EventMsg::TurnStarted(_)
                );
                let should_redraw_after_event = matches!(event.msg, EventMsg::ExecCommandEnd(_));

                match &event.msg {
                    EventMsg::TurnComplete(_) => {
                        self.exit_reason = ExitReason::Completed;
                        self.exit_after_next_draw = true;
                        tui.frame_requester().schedule_frame();
                    }
                    EventMsg::TurnAborted(_) => {
                        self.exit_reason = ExitReason::UserRequested;
                        self.exit_after_next_draw = true;
                        tui.frame_requester().schedule_frame();
                    }
                    EventMsg::Error(ev) => {
                        // The error itself is rendered into the transcript; return a fatal exit
                        // reason so callers can exit non-zero without duplicating the message.
                        self.exit_reason = ExitReason::Fatal(ev.message.clone());
                        self.exit_after_next_draw = true;
                        tui.frame_requester().schedule_frame();
                    }
                    _ => {}
                }

                self.processor.handle_codex_event(event);
                if should_update_context {
                    self.update_bottom_pane_context_window();
                }
                if should_redraw_after_event {
                    tui.frame_requester().schedule_frame();
                }
                if should_stop_footer {
                    self.bottom_pane.set_task_running(false);
                }
            }
            AppEvent::CodexOp(op) => match op {
                Op::GetHistoryEntryRequest { offset, log_id } => {
                    let entry = self.prompt_history.lookup_text(log_id, offset);
                    if self
                        .bottom_pane
                        .composer_mut()
                        .on_history_entry_response(log_id, offset, entry)
                    {
                        tui.frame_requester().schedule_frame();
                    }
                }
                _ => {
                    let _ = self.codex_op_tx.send(op);
                }
            },
            AppEvent::StartFileSearch(query) => {
                self.file_search.on_user_query(query);
            }
            AppEvent::FileSearchResult { query, matches } => {
                self.bottom_pane
                    .composer_mut()
                    .on_file_search_result(query, matches);
                tui.frame_requester().schedule_frame();
            }
            AppEvent::FatalExitRequest(message) => {
                self.exit_reason = ExitReason::Fatal(message);
                self.bottom_pane.set_task_running(false);
                self.exit_after_next_draw = true;
                tui.frame_requester().schedule_frame();
            }
        }

        Ok(())
    }

    fn update_bottom_pane_context_window(&mut self) {
        let Some(context_window) = self.processor.model_context_window else {
            self.bottom_pane
                .set_context_window(None, Some(self.processor.token_usage.total_tokens));
            return;
        };
        if context_window <= 0 {
            self.bottom_pane
                .set_context_window(None, Some(self.processor.token_usage.total_tokens));
            return;
        }

        let percent_left = self
            .processor
            .context_usage
            .percent_of_context_window_remaining(context_window);
        self.bottom_pane
            .set_context_window(Some(percent_left), None);
    }
}

fn should_filter_thinking_event(msg: &EventMsg) -> bool {
    matches!(
        msg,
        EventMsg::AgentReasoning(_)
            | EventMsg::AgentReasoningDelta(_)
            | EventMsg::AgentReasoningRawContent(_)
            | EventMsg::AgentReasoningRawContentDelta(_)
            | EventMsg::AgentReasoningSectionBreak(_)
    )
}

// Extract the first bold (Markdown) element in the form **...** from `s`.
// Returns the inner text if found; otherwise `None`.
fn extract_first_bold(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] == b'*' && bytes[i + 1] == b'*' {
            let start = i + 2;
            let mut j = start;
            while j + 1 < bytes.len() {
                if bytes[j] == b'*' && bytes[j + 1] == b'*' {
                    let trimmed = s[start..j].trim();
                    return (!trimmed.is_empty()).then(|| trimmed.to_string());
                }
                j += 1;
            }
            return None;
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::insert_history::insert_history_lines;
    use crate::test_backend::VT100Backend;
    use codex_protocol::ThreadId;
    use codex_protocol::parse_command::ParsedCommand;
    use codex_protocol::protocol::AgentMessageDeltaEvent;
    use codex_protocol::protocol::AgentMessageEvent;
    use codex_protocol::protocol::ContextCompactedEvent;
    use codex_protocol::protocol::ExecCommandEndEvent;
    use codex_protocol::protocol::ExecCommandSource;
    use codex_protocol::protocol::PatchApplyEndEvent;
    use codex_protocol::protocol::SessionConfiguredEvent;
    use codex_protocol::protocol::TokenCountEvent;
    use codex_protocol::protocol::TokenUsageInfo;
    use codex_protocol::protocol::TurnCompleteEvent;
    use insta::assert_snapshot;
    use ratatui::layout::Rect;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Instant;
    use tokio::sync::mpsc::UnboundedReceiver;
    use tokio::sync::mpsc::unbounded_channel;

    fn line_to_plain_string(line: &ratatui::text::Line<'_>) -> String {
        let mut out = String::new();
        for span in &line.spans {
            out.push_str(span.content.as_ref());
        }
        out
    }

    fn lines_to_plain_strings(lines: &[ratatui::text::Line<'_>]) -> Vec<String> {
        lines.iter().map(line_to_plain_string).collect()
    }

    fn drain_history_cell_strings(
        rx: &mut UnboundedReceiver<AppEvent>,
        width: u16,
    ) -> Vec<Vec<String>> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            let AppEvent::InsertHistoryCell(cell) = ev else {
                continue;
            };
            out.push(lines_to_plain_strings(&cell.display_lines(width)));
        }
        out
    }

    fn drain_render_history_events(
        rx: &mut UnboundedReceiver<AppEvent>,
        terminal: &mut crate::custom_terminal::Terminal<VT100Backend>,
        width: u16,
        has_emitted_history_lines: &mut bool,
    ) {
        while let Ok(ev) = rx.try_recv() {
            let AppEvent::InsertHistoryCell(cell) = ev else {
                continue;
            };

            let cell: Arc<dyn HistoryCell> = cell.into();
            let mut display = cell.display_lines(width);
            if display.is_empty() {
                continue;
            }

            if !cell.is_stream_continuation() {
                if *has_emitted_history_lines {
                    display.insert(0, Line::from(""));
                } else {
                    *has_emitted_history_lines = true;
                }
            }

            insert_history_lines(terminal, display).expect("insert history");
        }
    }

    fn drive_stream_to_idle(
        proc: &mut RenderOnlyProcessor,
        rx: &mut UnboundedReceiver<AppEvent>,
        terminal: &mut crate::custom_terminal::Terminal<VT100Backend>,
        width: u16,
        has_emitted_history_lines: &mut bool,
    ) {
        for _ in 0..100 {
            proc.on_commit_tick();
            drain_render_history_events(rx, terminal, width, has_emitted_history_lines);
        }
    }

    fn make_render_only_processor(
        prompt: &str,
    ) -> (RenderOnlyProcessor, UnboundedReceiver<AppEvent>) {
        let (tx_raw, rx) = unbounded_channel::<AppEvent>();
        let app_event_tx = AppEventSender::new(tx_raw);
        let proc = RenderOnlyProcessor::new(app_event_tx);
        proc.emit_user_prompt(prompt.to_string());
        (proc, rx)
    }

    fn make_render_only_processor_without_prompt()
    -> (RenderOnlyProcessor, UnboundedReceiver<AppEvent>) {
        let (tx_raw, rx) = unbounded_channel::<AppEvent>();
        let app_event_tx = AppEventSender::new(tx_raw);
        (RenderOnlyProcessor::new(app_event_tx), rx)
    }

    #[tokio::test]
    async fn should_filter_thinking_events() {
        assert!(should_filter_thinking_event(
            &EventMsg::AgentReasoningDelta(codex_protocol::protocol::AgentReasoningDeltaEvent {
                delta: "thinking".to_string(),
            })
        ));
        assert!(!should_filter_thinking_event(&EventMsg::AgentMessageDelta(
            codex_protocol::protocol::AgentMessageDeltaEvent {
                delta: "output".to_string(),
            }
        )));
    }

    #[test]
    fn extract_first_bold_returns_first_markdown_bold_span() {
        assert_eq!(
            extract_first_bold("**Inspecting for code duplication**\n\nmore"),
            Some("Inspecting for code duplication".to_string())
        );
        assert_eq!(extract_first_bold("no bold here"), None);
        assert_eq!(extract_first_bold("**"), None);
        assert_eq!(extract_first_bold("**  ** trailing"), None);
        assert_eq!(
            extract_first_bold("prefix **first** then **second**"),
            Some("first".to_string())
        );
    }

    #[test]
    fn reasoning_delta_updates_status_header_from_first_bold() {
        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let app_event_tx = AppEventSender::new(tx_raw);
        let mut bottom_pane = BottomPane::new(BottomPaneParams {
            frame_requester: crate::tui::FrameRequester::test_dummy(),
            enhanced_keys_supported: false,
            app_event_tx,
            animations_enabled: false,
            placeholder_text: "Assign new task to CodexPotter".to_string(),
            disable_paste_burst: false,
        });
        bottom_pane.set_task_running(true);

        let mut tracker = ReasoningStatusTracker::new();
        assert!(
            tracker.on_delta("**Inspecting").is_none(),
            "incomplete header"
        );

        let Some(header) = tracker.on_delta(" for code duplication**") else {
            panic!("expected a header after receiving closing **");
        };
        bottom_pane.update_status_header(header);

        let status = bottom_pane.status_indicator().expect("status indicator");
        assert_eq!(status.header(), "Inspecting for code duplication");
    }

    #[test]
    fn render_only_context_window_percent_uses_baseline_and_last_token_usage() {
        let (tx_raw, _rx_app) = unbounded_channel::<AppEvent>();
        let app_event_tx = AppEventSender::new(tx_raw);

        let processor = RenderOnlyProcessor::new(app_event_tx.clone());
        let (op_tx, _op_rx) = unbounded_channel::<Op>();
        let bottom_pane = BottomPane::new(BottomPaneParams {
            frame_requester: crate::tui::FrameRequester::test_dummy(),
            enhanced_keys_supported: false,
            app_event_tx: app_event_tx.clone(),
            animations_enabled: false,
            placeholder_text: "Assign new task to CodexPotter".to_string(),
            disable_paste_burst: false,
        });
        let file_search = FileSearchManager::new(std::env::temp_dir(), app_event_tx.clone());
        let mut app = RenderAppState::new(
            processor,
            app_event_tx,
            op_tx,
            bottom_pane,
            crate::prompt_history_store::PromptHistoryStore::new(),
            file_search,
            VecDeque::new(),
        );

        app.processor.handle_codex_event(Event {
            id: "token-count".into(),
            msg: EventMsg::TokenCount(TokenCountEvent {
                info: Some(TokenUsageInfo {
                    // Simulate cumulative billing usage (should not drive the context window percent).
                    total_token_usage: TokenUsage {
                        total_tokens: 100_000,
                        ..TokenUsage::default()
                    },
                    // Simulate Codex's estimated tokens currently in the context window.
                    last_token_usage: TokenUsage {
                        total_tokens: 20_000,
                        ..TokenUsage::default()
                    },
                    model_context_window: Some(128_000),
                }),
                rate_limits: None,
            }),
        });

        app.update_bottom_pane_context_window();

        assert_eq!(app.bottom_pane.context_window_percent(), Some(93));
        assert_eq!(app.bottom_pane.context_window_used_tokens(), None);
        assert_eq!(app.processor.token_usage.total_tokens, 100_000);
    }

    #[test]
    fn render_only_context_window_fallback_shows_used_tokens() {
        let (tx_raw, _rx_app) = unbounded_channel::<AppEvent>();
        let app_event_tx = AppEventSender::new(tx_raw);

        let processor = RenderOnlyProcessor::new(app_event_tx.clone());
        let (op_tx, _op_rx) = unbounded_channel::<Op>();
        let bottom_pane = BottomPane::new(BottomPaneParams {
            frame_requester: crate::tui::FrameRequester::test_dummy(),
            enhanced_keys_supported: false,
            app_event_tx: app_event_tx.clone(),
            animations_enabled: false,
            placeholder_text: "Assign new task to CodexPotter".to_string(),
            disable_paste_burst: false,
        });
        let file_search = FileSearchManager::new(std::env::temp_dir(), app_event_tx.clone());
        let mut app = RenderAppState::new(
            processor,
            app_event_tx,
            op_tx,
            bottom_pane,
            crate::prompt_history_store::PromptHistoryStore::new(),
            file_search,
            VecDeque::new(),
        );

        app.processor.token_usage = TokenUsage {
            total_tokens: 123_456,
            ..TokenUsage::default()
        };
        app.processor.model_context_window = None;

        app.update_bottom_pane_context_window();

        assert_eq!(app.bottom_pane.context_window_percent(), None);
        assert_eq!(app.bottom_pane.context_window_used_tokens(), Some(123_456));
    }

    #[test]
    fn render_only_composer_processes_repeat_cursor_movement() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyEventKind;
        use crossterm::event::KeyModifiers;

        let (tx_raw, _rx_app) = unbounded_channel::<AppEvent>();
        let app_event_tx = AppEventSender::new(tx_raw);

        let processor = RenderOnlyProcessor::new(app_event_tx.clone());
        let (op_tx, _op_rx) = unbounded_channel::<Op>();
        let bottom_pane = BottomPane::new(BottomPaneParams {
            frame_requester: crate::tui::FrameRequester::test_dummy(),
            enhanced_keys_supported: false,
            app_event_tx: app_event_tx.clone(),
            animations_enabled: false,
            placeholder_text: "Assign new task to CodexPotter".to_string(),
            disable_paste_burst: false,
        });
        let file_search = FileSearchManager::new(std::env::temp_dir(), app_event_tx.clone());
        let mut app = RenderAppState::new(
            processor,
            app_event_tx,
            op_tx,
            bottom_pane,
            crate::prompt_history_store::PromptHistoryStore::new(),
            file_search,
            VecDeque::new(),
        );

        app.bottom_pane
            .composer_mut()
            .set_text_content("hello".to_string());
        let area = Rect::new(0, 0, 80, 10);
        let before =
            crate::render::renderable::Renderable::cursor_pos(&app.bottom_pane, area).unwrap();

        let mut right_repeat = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        right_repeat.kind = KeyEventKind::Repeat;
        app.handle_key_event(right_repeat, crate::tui::FrameRequester::test_dummy());

        let after =
            crate::render::renderable::Renderable::cursor_pos(&app.bottom_pane, area).unwrap();
        assert!(
            after.0 > before.0,
            "expected cursor to move right on Repeat (before={before:?}, after={after:?})",
        );
    }

    #[test]
    fn render_only_composer_processes_repeat_ctrl_w() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyEventKind;
        use crossterm::event::KeyModifiers;

        let (tx_raw, _rx_app) = unbounded_channel::<AppEvent>();
        let app_event_tx = AppEventSender::new(tx_raw);

        let processor = RenderOnlyProcessor::new(app_event_tx.clone());
        let (op_tx, _op_rx) = unbounded_channel::<Op>();
        let bottom_pane = BottomPane::new(BottomPaneParams {
            frame_requester: crate::tui::FrameRequester::test_dummy(),
            enhanced_keys_supported: false,
            app_event_tx: app_event_tx.clone(),
            animations_enabled: false,
            placeholder_text: "Assign new task to CodexPotter".to_string(),
            disable_paste_burst: false,
        });
        let file_search = FileSearchManager::new(std::env::temp_dir(), app_event_tx.clone());
        let mut app = RenderAppState::new(
            processor,
            app_event_tx,
            op_tx,
            bottom_pane,
            crate::prompt_history_store::PromptHistoryStore::new(),
            file_search,
            VecDeque::new(),
        );

        app.bottom_pane.composer_mut().set_disable_paste_burst(true);
        for ch in "hello world".chars() {
            app.handle_key_event(
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                crate::tui::FrameRequester::test_dummy(),
            );
        }
        assert_eq!(app.bottom_pane.composer().current_text(), "hello world");

        let mut ctrl_w_repeat = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL);
        ctrl_w_repeat.kind = KeyEventKind::Repeat;
        app.handle_key_event(ctrl_w_repeat, crate::tui::FrameRequester::test_dummy());

        assert_eq!(app.bottom_pane.composer().current_text(), "hello ");
    }

    #[tokio::test]
    async fn render_only_renders_context_compacted_event() {
        let width: u16 = 80;
        let height: u16 = 12;
        let backend = VT100Backend::new(width, height);
        let mut terminal =
            crate::custom_terminal::Terminal::with_options(backend).expect("create terminal");
        terminal.set_viewport_area(Rect::new(0, height - 1, width, 1));

        let (mut proc, mut rx) = make_render_only_processor("Explain this: `1 + 1`.");

        let configured = SessionConfiguredEvent {
            session_id: ThreadId::new(),
            forked_from_id: None,
            model: "test-model".to_string(),
            model_provider_id: "test-provider".to_string(),
            cwd: PathBuf::from("project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            rollout_path: PathBuf::from("rollout.jsonl"),
        };

        proc.handle_codex_event(Event {
            id: "session".into(),
            msg: EventMsg::SessionConfigured(configured),
        });

        let mut has_emitted_history_lines = false;
        drain_render_history_events(
            &mut rx,
            &mut terminal,
            width,
            &mut has_emitted_history_lines,
        );

        proc.handle_codex_event(Event {
            id: "context-compacted".into(),
            msg: EventMsg::ContextCompacted(ContextCompactedEvent),
        });

        drain_render_history_events(
            &mut rx,
            &mut terminal,
            width,
            &mut has_emitted_history_lines,
        );

        assert_snapshot!(
            "render_only_context_compacted_vt100",
            terminal.backend().vt100().screen().contents()
        );
    }

    #[tokio::test]
    async fn render_only_vt100_snapshots() {
        let width: u16 = 80;
        let height: u16 = 28;
        let backend = VT100Backend::new(width, height);
        let mut terminal =
            crate::custom_terminal::Terminal::with_options(backend).expect("create terminal");
        terminal.set_viewport_area(Rect::new(0, height - 1, width, 1));

        let (mut proc, mut rx) = make_render_only_processor("Explain this: `1 + 1`.");

        let configured = SessionConfiguredEvent {
            session_id: ThreadId::new(),
            forked_from_id: None,
            model: "test-model".to_string(),
            model_provider_id: "test-provider".to_string(),
            cwd: PathBuf::from("project"),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            rollout_path: PathBuf::from("rollout.jsonl"),
        };

        proc.handle_codex_event(Event {
            id: "session".into(),
            msg: EventMsg::SessionConfigured(configured),
        });

        let mut has_emitted_history_lines = false;
        drain_render_history_events(
            &mut rx,
            &mut terminal,
            width,
            &mut has_emitted_history_lines,
        );

        // Stream markdown output in a few chunks to exercise incremental rendering.
        proc.handle_codex_event(Event {
            id: "delta-1".into(),
            msg: EventMsg::AgentMessageDelta(AgentMessageDeltaEvent {
                delta: "## Result\n".into(),
            }),
        });
        drive_stream_to_idle(
            &mut proc,
            &mut rx,
            &mut terminal,
            width,
            &mut has_emitted_history_lines,
        );

        proc.handle_codex_event(Event {
            id: "delta-2".into(),
            msg: EventMsg::AgentMessageDelta(AgentMessageDeltaEvent {
                delta: "- **Answer**: `2`\n".into(),
            }),
        });
        drive_stream_to_idle(
            &mut proc,
            &mut rx,
            &mut terminal,
            width,
            &mut has_emitted_history_lines,
        );

        assert_snapshot!(
            "render_only_streaming_partial_vt100",
            terminal.backend().vt100().screen().contents()
        );

        proc.handle_codex_event(Event {
            id: "delta-3".into(),
            msg: EventMsg::AgentMessageDelta(AgentMessageDeltaEvent {
                delta: "\n```sh\nprintf 'hello\\n'\n```\n".into(),
            }),
        });
        drive_stream_to_idle(
            &mut proc,
            &mut rx,
            &mut terminal,
            width,
            &mut has_emitted_history_lines,
        );

        // Finalize stream without a final AgentMessage, matching the streaming-only code path.
        proc.handle_codex_event(Event {
            id: "turn-complete".into(),
            msg: EventMsg::TurnComplete(TurnCompleteEvent {
                last_agent_message: None,
            }),
        });
        drain_render_history_events(
            &mut rx,
            &mut terminal,
            width,
            &mut has_emitted_history_lines,
        );

        // Exec output should render with truncation.
        let command = vec!["bash".into(), "-lc".into(), "printf 'line\\n'".into()];
        proc.handle_codex_event(Event {
            id: "exec-end".into(),
            msg: EventMsg::ExecCommandEnd(ExecCommandEndEvent {
                call_id: "exec-1".into(),
                process_id: None,
                turn_id: "turn-1".into(),
                command,
                cwd: PathBuf::from("project"),
                parsed_cmd: Vec::new(),
                source: ExecCommandSource::Agent,
                interaction_input: None,
                stdout: String::new(),
                stderr: String::new(),
                aggregated_output: (1..=30).map(|i| format!("line {i}\n")).collect::<String>(),
                exit_code: 0,
                duration: std::time::Duration::from_millis(1200),
                formatted_output: String::new(),
            }),
        });
        drain_render_history_events(
            &mut rx,
            &mut terminal,
            width,
            &mut has_emitted_history_lines,
        );

        // Patch apply should render a diff summary.
        let patch = diffy::create_patch("old\n", "new\n").to_string();
        let mut changes: HashMap<PathBuf, codex_protocol::protocol::FileChange> = HashMap::new();
        changes.insert(
            PathBuf::from("example.txt"),
            codex_protocol::protocol::FileChange::Update {
                unified_diff: patch,
                move_path: None,
            },
        );

        proc.handle_codex_event(Event {
            id: "patch-end".into(),
            msg: EventMsg::PatchApplyEnd(PatchApplyEndEvent {
                call_id: "patch-1".into(),
                turn_id: "turn-1".into(),
                stdout: String::new(),
                stderr: String::new(),
                success: true,
                changes,
            }),
        });
        drain_render_history_events(
            &mut rx,
            &mut terminal,
            width,
            &mut has_emitted_history_lines,
        );

        assert_snapshot!(
            "render_only_end_to_end_vt100",
            terminal.backend().vt100().screen().contents()
        );
    }

    #[test]
    fn render_only_live_explored_renders_in_viewport_and_merges_calls_vt100() {
        let width: u16 = 80;

        let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
        let app_event_tx = AppEventSender::new(tx_raw);

        let mut proc = RenderOnlyProcessor::new(app_event_tx);

        let base = ExecCommandEndEvent {
            call_id: "unused".into(),
            process_id: None,
            turn_id: "turn-1".into(),
            command: vec!["bash".into(), "-lc".into(), "true".into()],
            cwd: PathBuf::from("project"),
            parsed_cmd: Vec::new(),
            source: ExecCommandSource::Agent,
            interaction_input: None,
            stdout: String::new(),
            stderr: String::new(),
            aggregated_output: String::new(),
            exit_code: 0,
            duration: std::time::Duration::from_millis(1),
            formatted_output: String::new(),
        };

        // Simulate a burst of "exploring" exec results arriving over time.
        for (id, parsed_cmd) in [
            (
                "explore-1",
                vec![ParsedCommand::ListFiles {
                    cmd: "ls".into(),
                    path: None,
                }],
            ),
            (
                "explore-2",
                vec![ParsedCommand::ListFiles {
                    cmd: "ls -la".into(),
                    path: Some(".codexpotter".into()),
                }],
            ),
            (
                "explore-3",
                vec![ParsedCommand::Read {
                    cmd: "cat".into(),
                    name: "README.md".into(),
                    path: PathBuf::from("README.md"),
                }],
            ),
            (
                "explore-4",
                vec![ParsedCommand::Read {
                    cmd: "cat".into(),
                    name: "MAIN.md".into(),
                    path: PathBuf::from("MAIN.md"),
                }],
            ),
            (
                "explore-5",
                vec![ParsedCommand::Search {
                    cmd: "rg -n \"KeyCode::Tab\"".into(),
                    query: Some("KeyCode::Tab|\\\\bTab\\\\b".into()),
                    path: Some("cli".into()),
                }],
            ),
        ] {
            proc.handle_codex_event(Event {
                id: id.into(),
                msg: EventMsg::ExecCommandEnd(ExecCommandEndEvent {
                    call_id: id.into(),
                    parsed_cmd,
                    ..base.clone()
                }),
            });
        }

        // No history cell events should have been emitted yet; explored output should render
        // live in the viewport instead.
        assert!(rx.try_recv().is_err());

        let Some(explored) = proc.pending_exploring_cell.as_ref() else {
            panic!("expected a pending explored cell");
        };
        let mut exploring_lines = Vec::new();
        exploring_lines.push(Line::from(""));
        exploring_lines.extend(explored.display_lines(width));

        let prompt_lines =
            history_cell::new_user_prompt("test prompt".to_string()).display_lines(width);

        let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
        let app_event_tx = AppEventSender::new(tx_raw);
        let mut bottom_pane = BottomPane::new(BottomPaneParams {
            frame_requester: crate::tui::FrameRequester::test_dummy(),
            enhanced_keys_supported: false,
            app_event_tx,
            animations_enabled: false,
            placeholder_text: "Assign new task to CodexPotter".to_string(),
            disable_paste_burst: false,
        });
        bottom_pane.set_task_running(true);
        if let Some(status) = bottom_pane.status_indicator_mut() {
            // Ensure the elapsed timer stays at 0s for a stable snapshot.
            status.pause_timer_at(Instant::now());
        }

        let pane_height = bottom_pane.desired_height(width).max(1);
        let exploring_height = u16::try_from(exploring_lines.len()).unwrap_or(u16::MAX);
        let viewport_height = pane_height.saturating_add(exploring_height);
        let prompt_height = u16::try_from(prompt_lines.len()).unwrap_or(u16::MAX);
        let height = prompt_height.saturating_add(viewport_height).max(1);

        let backend = VT100Backend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("create terminal");
        terminal
            .draw(|frame| {
                let area = frame.area();
                ratatui::widgets::Clear.render(area, frame.buffer_mut());

                let prompt_height = prompt_height.min(area.height);
                let prompt_area =
                    ratatui::layout::Rect::new(area.x, area.y, area.width, prompt_height);
                let viewport_area = ratatui::layout::Rect::new(
                    area.x,
                    area.y + prompt_height,
                    area.width,
                    area.height.saturating_sub(prompt_height),
                );

                ratatui::widgets::Paragraph::new(ratatui::text::Text::from(prompt_lines))
                    .render(prompt_area, frame.buffer_mut());
                render_render_only_viewport(
                    viewport_area,
                    frame.buffer_mut(),
                    &bottom_pane,
                    exploring_lines,
                );
            })
            .expect("draw");

        assert_snapshot!(
            "render_only_live_explored_in_viewport_vt100",
            terminal.backend().vt100().screen().contents()
        );
    }

    #[test]
    fn render_only_ctrl_c_preserves_pending_explored_output_vt100() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyEventKind;
        use crossterm::event::KeyEventState;
        use crossterm::event::KeyModifiers;

        let width: u16 = 80;
        let height: u16 = 18;
        let backend = VT100Backend::new(width, height);
        let mut terminal =
            crate::custom_terminal::Terminal::with_options(backend).expect("create terminal");
        terminal.set_viewport_area(Rect::new(0, height - 6, width, 6));

        let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
        let app_event_tx = AppEventSender::new(tx_raw);

        let mut proc = RenderOnlyProcessor::new(app_event_tx.clone());
        proc.handle_codex_event(Event {
            id: "session-start".into(),
            msg: EventMsg::PotterSessionStarted {
                user_message: None,
                working_dir: PathBuf::from("project"),
                project_dir: PathBuf::from(".codexpotter/projects/20260129_18"),
                user_prompt_file: PathBuf::from(".codexpotter/projects/20260129_18/MAIN.md"),
            },
        });
        proc.handle_codex_event(Event {
            id: "round-start".into(),
            msg: EventMsg::PotterRoundStarted {
                current: 1,
                total: 10,
            },
        });

        let mut has_emitted_history_lines = false;
        drain_render_history_events(
            &mut rx,
            &mut terminal,
            width,
            &mut has_emitted_history_lines,
        );

        let base = ExecCommandEndEvent {
            call_id: "unused".into(),
            process_id: None,
            turn_id: "turn-1".into(),
            command: vec!["bash".into(), "-lc".into(), "true".into()],
            cwd: PathBuf::from("project"),
            parsed_cmd: Vec::new(),
            source: ExecCommandSource::Agent,
            interaction_input: None,
            stdout: String::new(),
            stderr: String::new(),
            aggregated_output: String::new(),
            exit_code: 0,
            duration: std::time::Duration::from_millis(1),
            formatted_output: String::new(),
        };

        proc.handle_codex_event(Event {
            id: "explore-1".into(),
            msg: EventMsg::ExecCommandEnd(ExecCommandEndEvent {
                call_id: "explore-1".into(),
                command: vec!["bash".into(), "-lc".into(), "ls -la".into()],
                parsed_cmd: vec![ParsedCommand::ListFiles {
                    cmd: "ls -la".into(),
                    path: None,
                }],
                ..base.clone()
            }),
        });
        proc.handle_codex_event(Event {
            id: "explore-2".into(),
            msg: EventMsg::ExecCommandEnd(ExecCommandEndEvent {
                call_id: "explore-2".into(),
                command: vec![
                    "bash".into(),
                    "-lc".into(),
                    "rg -n README.md .codexpotter".into(),
                ],
                parsed_cmd: vec![ParsedCommand::Search {
                    cmd: "rg -n README.md .codexpotter".into(),
                    query: Some("README.md".into()),
                    path: Some(".codexpotter".into()),
                }],
                ..base
            }),
        });

        assert!(rx.try_recv().is_err());
        assert!(proc.pending_exploring_cell.is_some());

        let (codex_op_tx, _codex_op_rx) = unbounded_channel::<Op>();
        let file_search_dir = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
        let file_search = FileSearchManager::new(file_search_dir, app_event_tx.clone());
        let bottom_pane = BottomPane::new(BottomPaneParams {
            frame_requester: crate::tui::FrameRequester::test_dummy(),
            enhanced_keys_supported: false,
            app_event_tx: app_event_tx.clone(),
            animations_enabled: false,
            placeholder_text: "Assign new task to CodexPotter".to_string(),
            disable_paste_burst: false,
        });
        let mut app = RenderAppState::new(
            proc,
            app_event_tx,
            codex_op_tx,
            bottom_pane,
            crate::prompt_history_store::PromptHistoryStore::new(),
            file_search,
            VecDeque::new(),
        );

        app.handle_key_event(
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            },
            crate::tui::FrameRequester::test_dummy(),
        );

        drain_render_history_events(
            &mut rx,
            &mut terminal,
            width,
            &mut has_emitted_history_lines,
        );

        crate::terminal_cleanup::clear_inline_viewport_for_exit(&mut terminal)
            .expect("clear viewport");

        assert_snapshot!(
            "render_only_ctrl_c_preserves_pending_explored_output_vt100",
            terminal.backend().vt100().screen().contents()
        );
    }

    #[test]
    fn render_only_coalesces_success_ran_cells_snapshot() {
        let width: u16 = 80;
        let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
        let app_event_tx = AppEventSender::new(tx_raw);

        let mut proc = RenderOnlyProcessor::new(app_event_tx);

        let base = ExecCommandEndEvent {
            call_id: "unused".into(),
            process_id: None,
            turn_id: "turn-1".into(),
            command: vec!["bash".into(), "-lc".into(), "true".into()],
            cwd: PathBuf::from("project"),
            parsed_cmd: Vec::new(),
            source: ExecCommandSource::Agent,
            interaction_input: None,
            stdout: String::new(),
            stderr: String::new(),
            aggregated_output: String::new(),
            exit_code: 0,
            duration: std::time::Duration::from_millis(1),
            formatted_output: String::new(),
        };

        for (id, inner) in [
            ("ran-1", "git status --porcelain=v1"),
            ("ran-2", "git --no-pager log -5 --oneline"),
        ] {
            proc.handle_codex_event(Event {
                id: id.into(),
                msg: EventMsg::ExecCommandEnd(ExecCommandEndEvent {
                    call_id: id.into(),
                    command: vec!["bash".into(), "-lc".into(), inner.into()],
                    ..base.clone()
                }),
            });
        }

        // Coalesced Ran output should render live (not emitted as transcript history yet).
        assert!(rx.try_recv().is_err());

        let Some(cell) = proc.pending_success_ran_cell.as_ref() else {
            panic!("expected a pending Ran cell");
        };
        let lines = cell.display_lines(width);
        assert_snapshot!(
            "render_only_coalesced_success_ran_cells",
            lines_to_plain_strings(&lines).join("\n")
        );
    }

    #[tokio::test]
    async fn render_only_coalesces_explored_cells() {
        let (mut proc, mut rx) = make_render_only_processor("test prompt");
        let _ = drain_history_cell_strings(&mut rx, u16::MAX);

        let base = ExecCommandEndEvent {
            call_id: "unused".into(),
            process_id: None,
            turn_id: "turn-1".into(),
            command: vec!["bash".into(), "-lc".into(), "true".into()],
            cwd: PathBuf::from("project"),
            parsed_cmd: Vec::new(),
            source: ExecCommandSource::Agent,
            interaction_input: None,
            stdout: String::new(),
            stderr: String::new(),
            aggregated_output: String::new(),
            exit_code: 0,
            duration: std::time::Duration::from_millis(1),
            formatted_output: String::new(),
        };

        proc.handle_codex_event(Event {
            id: "explore-1".into(),
            msg: EventMsg::ExecCommandEnd(ExecCommandEndEvent {
                call_id: "explore-1".into(),
                parsed_cmd: vec![ParsedCommand::Read {
                    cmd: "cat".into(),
                    name: "AGENTS.override.md".into(),
                    path: PathBuf::from("AGENTS.override.md"),
                }],
                ..base.clone()
            }),
        });
        proc.handle_codex_event(Event {
            id: "explore-2".into(),
            msg: EventMsg::ExecCommandEnd(ExecCommandEndEvent {
                call_id: "explore-2".into(),
                parsed_cmd: vec![ParsedCommand::ListFiles {
                    cmd: "ls -la".into(),
                    path: Some(".codexpotter".into()),
                }],
                ..base.clone()
            }),
        });
        proc.handle_codex_event(Event {
            id: "explore-3".into(),
            msg: EventMsg::ExecCommandEnd(ExecCommandEndEvent {
                call_id: "explore-3".into(),
                parsed_cmd: vec![ParsedCommand::ListFiles {
                    cmd: "ls -la".into(),
                    path: Some(".codexpotter".into()),
                }],
                ..base.clone()
            }),
        });
        proc.handle_codex_event(Event {
            id: "explore-4".into(),
            msg: EventMsg::ExecCommandEnd(ExecCommandEndEvent {
                call_id: "explore-4".into(),
                parsed_cmd: vec![ParsedCommand::Read {
                    cmd: "cat".into(),
                    name: "MAIN.md".into(),
                    path: PathBuf::from("MAIN.md"),
                }],
                ..base.clone()
            }),
        });
        proc.handle_codex_event(Event {
            id: "explore-5".into(),
            msg: EventMsg::ExecCommandEnd(ExecCommandEndEvent {
                call_id: "explore-5".into(),
                parsed_cmd: vec![ParsedCommand::Read {
                    cmd: "cat".into(),
                    name: "developer_prompt.md".into(),
                    path: PathBuf::from("developer_prompt.md"),
                }],
                ..base
            }),
        });

        // Any non-exec output should flush the buffered exploring cell.
        proc.handle_codex_event(Event {
            id: "agent-message".into(),
            msg: EventMsg::AgentMessage(AgentMessageEvent {
                message: "ok".into(),
            }),
        });

        let events = drain_history_cell_strings(&mut rx, u16::MAX);
        let [explored, _agent_message] = events.as_slice() else {
            panic!("expected explored cell followed by agent message");
        };
        let rendered = explored.join("\n") + "\n";
        assert_snapshot!("render_only_coalesces_explored_cells", rendered);
    }

    #[tokio::test]
    async fn render_only_flushes_explored_cells_on_turn_complete() {
        let (mut proc, mut rx) = make_render_only_processor("test prompt");
        let _ = drain_history_cell_strings(&mut rx, u16::MAX);

        proc.handle_codex_event(Event {
            id: "explore-1".into(),
            msg: EventMsg::ExecCommandEnd(ExecCommandEndEvent {
                call_id: "explore-1".into(),
                process_id: None,
                turn_id: "turn-1".into(),
                command: vec!["bash".into(), "-lc".into(), "true".into()],
                cwd: PathBuf::from("project"),
                parsed_cmd: vec![ParsedCommand::Read {
                    cmd: "cat".into(),
                    name: "AGENTS.override.md".into(),
                    path: PathBuf::from("AGENTS.override.md"),
                }],
                source: ExecCommandSource::Agent,
                interaction_input: None,
                stdout: String::new(),
                stderr: String::new(),
                aggregated_output: String::new(),
                exit_code: 0,
                duration: std::time::Duration::from_millis(1),
                formatted_output: String::new(),
            }),
        });
        proc.handle_codex_event(Event {
            id: "turn-complete".into(),
            msg: EventMsg::TurnComplete(TurnCompleteEvent {
                last_agent_message: None,
            }),
        });

        let events = drain_history_cell_strings(&mut rx, u16::MAX);
        let [explored] = events.as_slice() else {
            panic!("expected exactly one explored cell");
        };
        let rendered = explored.join("\n") + "\n";
        assert_snapshot!(
            "render_only_flushes_explored_cells_on_turn_complete",
            rendered
        );
    }

    #[test]
    fn render_only_potter_session_started_emits_user_prompt() {
        let (mut proc, mut rx) = make_render_only_processor_without_prompt();

        proc.handle_codex_event(Event {
            id: "potter-session-started".into(),
            msg: EventMsg::PotterSessionStarted {
                user_message: Some("test prompt".to_string()),
                working_dir: PathBuf::from("/workdir"),
                project_dir: PathBuf::from(".codexpotter/projects/20260129_11"),
                user_prompt_file: PathBuf::from(".codexpotter/projects/20260129_11/MAIN.md"),
            },
        });

        let events = drain_history_cell_strings(&mut rx, u16::MAX);
        let [prompt, project_hint] = events.as_slice() else {
            panic!("expected prompt cell followed by project hint cell");
        };

        let prompt_rendered = prompt.join("\n") + "\n";
        assert_snapshot!("render_only_potter_session_started", prompt_rendered);

        let hint_rendered = project_hint.join("\n") + "\n";
        assert_snapshot!(
            "render_only_potter_session_started_project_hint",
            hint_rendered
        );
    }

    #[test]
    fn render_only_potter_round_started_emits_round_banner() {
        let (mut proc, mut rx) = make_render_only_processor_without_prompt();

        proc.handle_codex_event(Event {
            id: "potter-round-started".into(),
            msg: EventMsg::PotterRoundStarted {
                current: 1,
                total: 15,
            },
        });

        let events = drain_history_cell_strings(&mut rx, u16::MAX);
        let [round_banner] = events.as_slice() else {
            panic!("expected exactly one round banner cell");
        };
        let rendered = round_banner.join("\n") + "\n";
        assert_snapshot!("render_only_potter_round_started", rendered);
    }

    fn render_prompt_footer_line(override_mode: Option<PromptFooterOverride>) -> String {
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        crate::bottom_pane::render_prompt_footer_for_test(area, &mut buf, override_mode);

        let mut out = String::new();
        for x in 0..area.width {
            out.push_str(buf[(x, 0)].symbol());
        }
        out.trim_end().to_string()
    }

    #[test]
    fn prompt_footer_includes_external_editor_hint() {
        assert_snapshot!(
            "prompt_footer_includes_external_editor",
            render_prompt_footer_line(None)
        );
    }

    #[test]
    fn prompt_footer_external_editor_override_replaces_footer() {
        assert_snapshot!(
            "prompt_footer_external_editor_override",
            render_prompt_footer_line(Some(PromptFooterOverride::ExternalEditorHint))
        );
    }
}
