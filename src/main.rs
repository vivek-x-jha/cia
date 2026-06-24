mod codex;
mod config;
mod model;
mod runner;
mod state;
mod tmux;
mod ui;

use std::{path::PathBuf, process::Command};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Preselect the project matching this working directory
    #[arg(long, value_name = "PATH")]
    project: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Resume a Codex thread while preserving tmux metadata
    #[command(hide = true)]
    RunThread {
        #[arg(long)]
        thread_id: Option<String>,
        #[arg(long)]
        cwd_hex: String,
        #[arg(long)]
        title_hex: String,
        #[arg(long)]
        codex_command_hex: String,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    let config = config::Config::load()?;
    if let Some(Commands::RunThread {
        thread_id,
        cwd_hex,
        title_hex,
        codex_command_hex,
    }) = args.command
    {
        let cwd = PathBuf::from(runner::decode(&cwd_hex, "working directory")?);
        let title = runner::decode(&title_hex, "chat title")?;
        let codex_command = runner::decode(&codex_command_hex, "Codex command")?;
        return run_thread(
            &config.tmux,
            thread_id.as_deref(),
            &cwd,
            &title,
            &codex_command,
        );
    }
    let app = ui::App::new(config, args.project)?;
    ui::run(app)
}

fn run_thread(
    tmux_config: &config::TmuxConfig,
    thread_id: Option<&str>,
    cwd: &PathBuf,
    title: &str,
    codex_command: &str,
) -> Result<()> {
    let pane_id = std::env::var_os("TMUX_PANE").map(|value| value.to_string_lossy().into_owned());
    let tmux = tmux::Client::new(tmux_config.clone());
    let existing_id = if thread_id.is_none() {
        let mut client = codex::Client::start(codex_command)?;
        let cwd = tmux::normalized_path(cwd).to_string_lossy().into_owned();
        client
            .list_threads(false)?
            .into_iter()
            .find(|thread| {
                thread.name.as_deref() == Some(title)
                    && tmux::normalized_path(&thread.cwd).to_string_lossy() == cwd
            })
            .map(|thread| thread.id)
    } else {
        None
    };
    let resume_id = thread_id.or(existing_id.as_deref());
    if let Some(pane_id) = &pane_id {
        if let Some(thread_id) = resume_id {
            tmux.mark_pane(pane_id, thread_id, title)?;
        } else {
            tmux.mark_title(pane_id, title)?;
        }
    }
    let mut command = Command::new(codex_command);
    if let Some(thread_id) = resume_id {
        command.arg("resume").arg(thread_id).arg("-C").arg(cwd);
    } else {
        command.arg("-C").arg(cwd);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to run {codex_command}"))?;
    if resume_id.is_none() {
        if let Some(pane_id) = &pane_id {
            tmux.rename_active_codex_thread(pane_id, title)?;
        }
    }
    let status = child.wait()?;
    if !status.success() {
        bail!("{codex_command} exited with {status}");
    }
    Ok(())
}
