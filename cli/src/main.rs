mod app_server_backend;
mod app_server_protocol;
mod atomic_write;
mod codex_compat;
mod config;
mod global_gitignore;
mod path_utils;
mod project;
mod prompt_queue;
mod startup;

use std::num::NonZeroUsize;

use anyhow::Context;
use chrono::Local;
use clap::Parser;
use clap::ValueEnum;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_tui::ExitReason;
use tokio::sync::mpsc::unbounded_channel;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
#[clap(rename_all = "kebab-case")]
enum CliSandbox {
    #[default]
    Default,
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl CliSandbox {
    fn as_protocol(self) -> Option<crate::app_server_protocol::SandboxMode> {
        match self {
            CliSandbox::Default => None,
            CliSandbox::ReadOnly => Some(crate::app_server_protocol::SandboxMode::ReadOnly),
            CliSandbox::WorkspaceWrite => {
                Some(crate::app_server_protocol::SandboxMode::WorkspaceWrite)
            }
            CliSandbox::DangerFullAccess => {
                Some(crate::app_server_protocol::SandboxMode::DangerFullAccess)
            }
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    author = "Codex",
    version,
    about = "Run a multi-round Codex workflow using legacy TUI formatting via app-server"
)]
struct Cli {
    /// Path to the `codex` CLI binary to launch in app-server mode.
    #[arg(long, env = "CODEX_BIN", default_value = "codex")]
    codex_bin: String,

    /// Number of turns to run (each turn starts a fresh `codex app-server`; must be >= 1).
    #[arg(long, default_value = "10")]
    rounds: NonZeroUsize,

    /// Sandbox mode to request from Codex.
    ///
    /// `default` matches codex-cli behavior: no `--sandbox` flag is passed to the app-server and
    /// the sandbox policy is left for Codex to decide.
    #[arg(long = "sandbox", value_enum, default_value_t)]
    sandbox: CliSandbox,

    /// Pass Codex's bypass flag when launching `codex app-server`.
    ///
    /// Alias: `--yolo`.
    #[arg(long = "dangerously-bypass-approvals-and-sandbox", alias = "yolo")]
    dangerously_bypass_approvals_and_sandbox: bool,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let bypass = cli.dangerously_bypass_approvals_and_sandbox;
    let sandbox = cli.sandbox;

    let codex_bin = match startup::resolve_codex_bin(&cli.codex_bin) {
        Ok(resolved) => resolved.command_for_spawn,
        Err(err) => {
            eprint!("{}", err.render_ansi());
            std::process::exit(1);
        }
    };

    let workdir = std::env::current_dir().context("resolve current directory")?;

    let codex_compat_home = match crate::codex_compat::ensure_default_codex_compat_home() {
        Ok(home) => home,
        Err(err) => {
            eprintln!("warning: failed to configure codex-compat home: {err}");
            None
        }
    };

    let global_gitignore_prompt_plan = prepare_global_gitignore_prompt(&workdir);
    let mut ui = codex_tui::CodexPotterTui::new()?;
    if let Some(plan) = global_gitignore_prompt_plan {
        maybe_prompt_global_gitignore(&mut ui, &workdir, plan).await;
    }
    let Some(user_prompt) = ui.prompt_user().await? else {
        return Ok(());
    };

    // Clear prompt UI remnants before doing any work / streaming output.
    ui.clear()?;

    let backend_launch = app_server_backend::AppServerLaunchConfig::from_cli(sandbox, bypass);

    let turn_prompt = crate::project::fixed_prompt().trim_end().to_string();

    let mut pending_user_prompts = prompt_queue::PromptQueue::new(user_prompt);

    'session: loop {
        let next_prompt = pending_user_prompts.pop_next_prompt(|| ui.pop_queued_user_prompt());
        let Some(next_prompt) =
            prompt_queue::next_prompt_or_prompt_user(next_prompt, || ui.prompt_user()).await?
        else {
            break 'session;
        };

        let user_prompt = match next_prompt {
            prompt_queue::NextPrompt::FromQueue(prompt) => prompt,
            prompt_queue::NextPrompt::FromUser(prompt) => {
                // Clear prompt UI remnants before doing any work / streaming output.
                ui.clear()?;
                prompt
            }
        };

        let init = crate::project::init_project(&workdir, &user_prompt, Local::now())
            .context("initialize .codexpotter project")?;
        let project_dir = init
            .progress_file_rel
            .parent()
            .context("derive CodexPotter project dir from progress file path")?
            .to_path_buf();
        let user_prompt_file = init.progress_file_rel.clone();
        let developer_prompt = crate::project::render_developer_prompt(&init.progress_file_rel);

        for round_index in 0..cli.rounds.get() {
            let (op_tx, op_rx) = unbounded_channel::<Op>();
            let (event_tx, event_rx) = unbounded_channel::<Event>();
            let (fatal_exit_tx, fatal_exit_rx) = unbounded_channel::<String>();

            let potter_event_tx = event_tx.clone();
            if round_index == 0 {
                let _ = potter_event_tx.send(Event {
                    id: "".to_string(),
                    msg: EventMsg::PotterSessionStarted {
                        user_message: Some(user_prompt.clone()),
                        working_dir: workdir.clone(),
                        project_dir: project_dir.clone(),
                        user_prompt_file: user_prompt_file.clone(),
                    },
                });
            }
            let total_rounds = u32::try_from(cli.rounds.get()).unwrap_or(u32::MAX);
            let current_round = u32::try_from(round_index.saturating_add(1)).unwrap_or(u32::MAX);
            let _ = potter_event_tx.send(Event {
                id: "".to_string(),
                msg: EventMsg::PotterRoundStarted {
                    current: current_round,
                    total: total_rounds,
                },
            });

            let backend = tokio::spawn(app_server_backend::run_app_server_backend(
                codex_bin.clone(),
                Some(developer_prompt.clone()),
                backend_launch,
                codex_compat_home.clone(),
                op_rx,
                event_tx,
                fatal_exit_tx,
            ));

            let exit_info = ui
                .render_turn(
                    turn_prompt.clone(),
                    round_index != 0,
                    op_tx,
                    event_rx,
                    fatal_exit_rx,
                )
                .await?;

            match &exit_info.exit_reason {
                ExitReason::UserRequested => {
                    backend.abort();
                    let _ = backend.await;
                    break 'session;
                }
                ExitReason::Fatal(_) => {
                    backend.abort();
                    let _ = backend.await;
                    // `std::process::exit` skips destructors, so explicitly drop the UI to restore
                    // terminal state before exiting.
                    drop(ui);
                    std::process::exit(1);
                }
                ExitReason::Completed => {}
            }

            backend
                .await
                .context("app-server render backend panicked")??;
            if crate::project::progress_file_has_finite_incantatem_true(
                &workdir,
                &init.progress_file_rel,
            )
            .context("check progress file finite_incantatem")?
            {
                break;
            }
        }
    }

    Ok(())
}

struct GlobalGitignorePromptPlan {
    config_store: crate::config::ConfigStore,
    status: crate::global_gitignore::GlobalGitignoreStatus,
}

fn prepare_global_gitignore_prompt(workdir: &std::path::Path) -> Option<GlobalGitignorePromptPlan> {
    let config_store = match crate::config::ConfigStore::new_default() {
        Ok(store) => store,
        Err(err) => {
            eprintln!("warning: failed to locate codexpotter config: {err}");
            return None;
        }
    };

    let hide_prompt = config_store.notice_hide_gitignore_prompt().unwrap_or(false);
    if hide_prompt {
        return None;
    }

    let status = match crate::global_gitignore::detect_global_gitignore(workdir) {
        Ok(status) => status,
        Err(err) => {
            eprintln!("warning: failed to resolve global gitignore: {err}");
            return None;
        }
    };
    if status.has_codexpotter_ignore {
        return None;
    }

    Some(GlobalGitignorePromptPlan {
        config_store,
        status,
    })
}

async fn maybe_prompt_global_gitignore(
    ui: &mut codex_tui::CodexPotterTui,
    workdir: &std::path::Path,
    plan: GlobalGitignorePromptPlan,
) {
    let outcome = match ui
        .prompt_global_gitignore(plan.status.path_display.clone())
        .await
    {
        Ok(outcome) => outcome,
        Err(err) => {
            eprintln!("warning: global gitignore prompt failed: {err}");
            let _ = ui.clear();
            return;
        }
    };

    match outcome {
        codex_tui::GlobalGitignorePromptOutcome::AddToGlobalGitignore => {
            if let Err(err) =
                crate::global_gitignore::ensure_codexpotter_ignored(workdir, &plan.status.path)
            {
                eprintln!("warning: failed to update global gitignore: {err}");
            }
        }
        codex_tui::GlobalGitignorePromptOutcome::No => {}
        codex_tui::GlobalGitignorePromptOutcome::NoDontAskAgain => {
            if let Err(err) = plan.config_store.set_notice_hide_gitignore_prompt(true) {
                eprintln!("warning: failed to persist config: {err}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_must_be_at_least_one() {
        assert!(Cli::try_parse_from(["codex-potter", "--rounds", "0"]).is_err());
        assert!(Cli::try_parse_from(["codex-potter", "--rounds", "1"]).is_ok());
    }

    #[test]
    fn yolo_alias_sets_bypass_flag() {
        let cli = Cli::try_parse_from(["codex-potter", "--yolo"]).expect("parse args");
        assert!(cli.dangerously_bypass_approvals_and_sandbox);
    }
}
