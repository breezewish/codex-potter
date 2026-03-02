mod app_server_backend;
mod app_server_protocol;
mod atomic_write;
mod codex_compat;
mod config;
mod global_gitignore;
mod path_utils;
mod potter_rollout;
mod potter_rollout_resume_index;
mod potter_stream_recovery;
mod project;
mod prompt_queue;
mod resume;
mod resume_picker_index;
mod round_runner;
mod startup;

use std::num::NonZeroUsize;
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Context;
use chrono::Local;
use clap::CommandFactory;
use clap::FromArgMatches;
use clap::Parser;
use clap::Subcommand;
use clap::ValueEnum;
use codex_tui::ExitReason;

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
#[command(author = "Codex", version, about = "Run CodexPotter interactively")]
struct Cli {
    /// Path to the `codex` CLI binary to launch in app-server mode.
    #[arg(long, env = "CODEX_BIN", default_value = "codex", global = true)]
    codex_bin: String,

    /// Number of turns to run (each turn starts a fresh `codex app-server`; must be >= 1).
    ///
    /// For `resume`, this controls how many rounds are run when the last recorded round is
    /// complete. If the last recorded round is unfinished, the remaining budget is derived from
    /// the recorded `round_total` in `potter-rollout.jsonl`.
    #[arg(long, default_value = "10", global = true)]
    rounds: NonZeroUsize,

    /// Sandbox mode to request from Codex.
    ///
    /// `default` matches codex-cli behavior: no `--sandbox` flag is passed to the app-server and
    /// the sandbox policy is left for Codex to decide.
    #[arg(long = "sandbox", value_enum, default_value_t, global = true)]
    sandbox: CliSandbox,

    /// Pass Codex's bypass flag when launching `codex app-server`.
    ///
    /// Alias: `--yolo`.
    #[arg(
        long = "dangerously-bypass-approvals-and-sandbox",
        alias = "yolo",
        global = true
    )]
    dangerously_bypass_approvals_and_sandbox: bool,

    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Subcommand, Debug)]
enum CliCommand {
    /// Resume a CodexPotter project (replay history and optionally continue iterating).
    Resume {
        /// Project path to resolve to a unique `MAIN.md`. If omitted, open a picker UI.
        project_path: Option<PathBuf>,
    },
}

fn parse_cli() -> Cli {
    let matches = Cli::command()
        .version(codex_tui::CODEX_POTTER_VERSION)
        .get_matches();
    Cli::from_arg_matches(&matches).unwrap_or_else(|err| err.exit())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = parse_cli();
    let bypass = cli.dangerously_bypass_approvals_and_sandbox;
    let sandbox = cli.sandbox;
    let mut resume_note_project_path: Option<String> = None;

    let check_for_update_on_startup = crate::config::ConfigStore::new_default()
        .and_then(|store| store.check_for_update_on_startup())
        .unwrap_or(true);

    let codex_bin = match startup::resolve_codex_bin(&cli.codex_bin) {
        Ok(resolved) => resolved.command_for_spawn,
        Err(err) => {
            eprint!("{}", err.render_ansi());
            std::process::exit(1);
        }
    };

    let workdir = std::env::current_dir().context("resolve current directory")?;

    let backend_launch = app_server_backend::AppServerLaunchConfig::from_cli(sandbox, bypass);
    let turn_prompt = crate::project::fixed_prompt().trim_end().to_string();

    let codex_compat_home = match crate::codex_compat::ensure_default_codex_compat_home() {
        Ok(home) => home,
        Err(err) => {
            eprintln!("warning: failed to configure codex-compat home: {err}");
            None
        }
    };

    let mut ui = codex_tui::CodexPotterTui::new()?;

    ui.set_check_for_update_on_startup(check_for_update_on_startup);
    if let Some(update_action) = ui.prompt_update_if_needed().await? {
        drop(ui);
        run_update_action(update_action)?;
        return Ok(());
    }

    let global_gitignore_prompt_plan = prepare_global_gitignore_prompt(&workdir);
    if let Some(plan) = global_gitignore_prompt_plan {
        maybe_prompt_global_gitignore(&mut ui, &workdir, plan).await;
    }

    if let Some(CliCommand::Resume { project_path }) = cli.command.as_ref() {
        let project_path = match project_path {
            Some(project_path) => Some(project_path.clone()),
            None => {
                let rows = crate::resume_picker_index::discover_resumable_projects(&workdir)
                    .context("discover resumable projects")?;
                match ui.prompt_resume_picker(rows).await? {
                    codex_tui::ResumePickerOutcome::StartFresh => None,
                    codex_tui::ResumePickerOutcome::Resume(project_path) => Some(project_path),
                    codex_tui::ResumePickerOutcome::Exit => return Ok(()),
                }
            }
        };

        if let Some(project_path) = project_path {
            let resume_exit = crate::resume::run_resume(
                &mut ui,
                &workdir,
                &project_path,
                codex_bin.clone(),
                backend_launch,
                codex_compat_home.clone(),
                cli.rounds,
            )
            .await
            .context("resume project")?;
            if resume_exit == crate::resume::ResumeExit::FatalExitRequested {
                // `std::process::exit` skips destructors, so explicitly drop the UI to restore terminal
                // state before exiting.
                drop(ui);
                std::process::exit(1);
            }
            return Ok(());
        }
    }

    let Some(user_prompt) = ui.prompt_user().await? else {
        return Ok(());
    };

    // Clear prompt UI remnants before doing any work / streaming output.
    ui.clear()?;

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
        let project_started_at = Instant::now();
        let project_dir = init
            .progress_file_rel
            .parent()
            .context("derive CodexPotter project dir from progress file path")?
            .to_path_buf();
        let project_dir_abs = workdir.join(&project_dir);
        let potter_rollout_path = crate::potter_rollout::potter_rollout_path(&project_dir_abs);
        let user_prompt_file = init.progress_file_rel.clone();
        let developer_prompt = crate::project::render_developer_prompt(&init.progress_file_rel);

        let round_context = crate::round_runner::PotterRoundContext {
            codex_bin: codex_bin.clone(),
            developer_prompt: developer_prompt.clone(),
            backend_launch,
            codex_compat_home: codex_compat_home.clone(),
            thread_cwd: Some(workdir.clone()),
            turn_prompt: turn_prompt.clone(),
            workdir: workdir.clone(),
            progress_file_rel: init.progress_file_rel.clone(),
            user_prompt_file: user_prompt_file.clone(),
            git_commit_start: init.git_commit_start.clone(),
            potter_rollout_path: potter_rollout_path.clone(),
            project_started_at,
        };

        for round_index in 0..cli.rounds.get() {
            let total_rounds = u32::try_from(cli.rounds.get()).unwrap_or(u32::MAX);
            let current_round = u32::try_from(round_index.saturating_add(1)).unwrap_or(u32::MAX);
            let session_started = if round_index == 0 {
                Some(crate::round_runner::PotterSessionStartedInfo {
                    user_message: Some(user_prompt.clone()),
                    working_dir: workdir.clone(),
                    project_dir: project_dir.clone(),
                    user_prompt_file: user_prompt_file.clone(),
                })
            } else {
                None
            };

            let round_result = crate::round_runner::run_potter_round(
                &mut ui,
                &round_context,
                crate::round_runner::PotterRoundOptions {
                    pad_before_first_cell: round_index != 0,
                    session_started,
                    round_current: current_round,
                    round_total: total_rounds,
                    session_succeeded_rounds: current_round,
                },
            )
            .await?;

            match &round_result.exit_reason {
                ExitReason::UserRequested => {
                    resume_note_project_path = Some(
                        derive_resume_project_path_from_project_dir(&project_dir)
                            .unwrap_or_else(|| project_dir.to_string_lossy().to_string()),
                    );
                    break 'session;
                }
                ExitReason::TaskFailed(_) => break,
                ExitReason::Fatal(_) => {
                    // `std::process::exit` skips destructors, so explicitly drop the UI to restore
                    // terminal state before exiting.
                    drop(ui);
                    std::process::exit(1);
                }
                ExitReason::Completed => {}
            }
            if round_result.stop_due_to_finite_incantatem {
                break;
            }
        }
    }

    drop(ui);
    if let Some(project_path) = resume_note_project_path {
        print_resume_note(&project_path);
    }

    Ok(())
}

fn run_update_action(action: codex_tui::UpdateAction) -> anyhow::Result<()> {
    println!();
    let cmd_str = action.command_str();
    println!("Updating CodexPotter via `{cmd_str}`...");

    let status = {
        #[cfg(windows)]
        {
            // On Windows, run via cmd.exe so .CMD/.BAT are correctly resolved (PATHEXT semantics).
            std::process::Command::new("cmd")
                .args(["/C", &cmd_str])
                .status()?
        }
        #[cfg(not(windows))]
        {
            let (cmd, args) = action.command_args();
            std::process::Command::new(cmd).args(args).status()?
        }
    };

    if !status.success() {
        anyhow::bail!("`{cmd_str}` failed with status {status}");
    }

    println!("Update ran successfully! Please restart CodexPotter.");
    Ok(())
}

fn derive_resume_project_path_from_project_dir(project_dir: &Path) -> Option<String> {
    let projects_root = Path::new(".codexpotter").join("projects");
    let project_path = project_dir.strip_prefix(&projects_root).ok()?;
    let parts = project_path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

fn print_resume_note(project_path: &str) {
    let command = format!("codex-potter resume {project_path}");
    println!("{} To continue this project, run:", ansi_bold("Note:"));
    println!("  {}", ansi_cyan(&command));
}

fn ansi_bold(text: &str) -> String {
    format!("\u{1b}[1m{text}\u{1b}[0m")
}

fn ansi_cyan(text: &str) -> String {
    format!("\u{1b}[36m{text}\u{1b}[0m")
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
    use pretty_assertions::assert_eq;

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

    #[test]
    fn resume_allows_global_args_after_subcommand() {
        let cli = Cli::try_parse_from([
            "codex-potter",
            "resume",
            "2026/02/01/1",
            "--yolo",
            "--sandbox",
            "read-only",
            "--rounds",
            "3",
            "--codex-bin",
            "custom-codex",
        ])
        .expect("parse args");

        assert!(cli.dangerously_bypass_approvals_and_sandbox);
        assert_eq!(cli.sandbox, CliSandbox::ReadOnly);
        assert_eq!(cli.rounds.get(), 3);
        assert_eq!(cli.codex_bin, "custom-codex");

        let Some(CliCommand::Resume { project_path }) = cli.command else {
            panic!("expected resume command, got: {:?}", cli.command);
        };
        assert_eq!(project_path, Some(PathBuf::from("2026/02/01/1")));
    }

    #[test]
    fn resume_subcommand_parses_project_path() {
        let cli =
            Cli::try_parse_from(["codex-potter", "resume", "2026/02/01/1"]).expect("parse args");

        let Some(CliCommand::Resume { project_path }) = cli.command else {
            panic!("expected resume command, got: {:?}", cli.command);
        };
        assert_eq!(project_path, Some(PathBuf::from("2026/02/01/1")));
    }

    #[test]
    fn resume_subcommand_parses_without_project_path() {
        let cli = Cli::try_parse_from(["codex-potter", "resume"]).expect("parse args");

        let Some(CliCommand::Resume { project_path }) = cli.command else {
            panic!("expected resume command, got: {:?}", cli.command);
        };
        assert_eq!(project_path, None);
    }

    #[test]
    fn derive_resume_project_path_from_project_dir_strips_projects_root() {
        let project_dir = Path::new(".codexpotter/projects/2026/03/01/6");
        assert_eq!(
            derive_resume_project_path_from_project_dir(project_dir),
            Some("2026/03/01/6".to_string())
        );
    }

    #[test]
    fn derive_resume_project_path_from_project_dir_returns_none_when_unexpected() {
        let project_dir = Path::new("not-a-project-dir");
        assert_eq!(
            derive_resume_project_path_from_project_dir(project_dir),
            None
        );
    }
}
