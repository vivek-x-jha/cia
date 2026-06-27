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
    #[arg(long, value_name = "PATH", global = true)]
    project: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Open the most recent saved thread matching a name or title
    Open {
        /// Thread name, title, or preview text to search for
        query: String,
        /// Search archived threads instead of active threads
        #[arg(long)]
        archived: bool,
    },

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
    match args.command {
        Some(Commands::Open { query, archived }) => {
            return open_thread_by_query(&config, args.project, &query, archived);
        }
        Some(Commands::RunThread {
            thread_id,
            cwd_hex,
            title_hex,
            codex_command_hex,
        }) => {
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
        None => {}
    }
    let app = ui::App::new(config, args.project)?;
    ui::run(app)
}

fn open_thread_by_query(
    config: &config::Config,
    project: Option<PathBuf>,
    query: &str,
    archived: bool,
) -> Result<()> {
    let project = project
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)
        .context("failed to determine current project directory")?;
    let project = tmux::normalized_path(project)
        .to_string_lossy()
        .into_owned();
    let mut codex = codex::Client::start(&config.codex.command)?;
    let project_threads: Vec<_> = codex
        .list_threads(archived)?
        .into_iter()
        .filter(|thread| tmux::normalized_path(&thread.cwd).to_string_lossy() == project)
        .collect();
    let thread = (0..=3)
        .find_map(|rank| {
            project_threads
                .iter()
                .find(|thread| thread_match_rank(thread, query) == Some(rank))
        })
        .with_context(|| {
            format!(
                "no {}thread matching `{query}` in {}",
                if archived { "archived " } else { "" },
                project
            )
        })?;

    let tmux = tmux::Client::new(config.tmux.clone());
    let mut state = state::State::load(&config::state_path())?;
    let mut windows = tmux.inventory()?;
    state.reconcile(&mut windows);
    if let Some(window) = windows
        .iter()
        .find(|window| window.thread_id.as_deref() == Some(thread.id.as_str()))
    {
        return tmux.switch_to(window);
    }

    let cia_command = std::env::current_exe()
        .context("failed to locate the CIA executable")?
        .to_string_lossy()
        .into_owned();
    let window = tmux.open_thread(
        &windows,
        &thread.cwd,
        thread.title(),
        &thread.id,
        &cia_command,
        &config.codex.command,
    )?;
    state.record(&thread.id, &window);
    state.last_project = Some(thread.cwd.clone());
    state.save(&config::state_path())?;
    tmux.switch_to(&window)
}

fn thread_match_rank(thread: &codex::Thread, query: &str) -> Option<u8> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    let title = thread.title();
    if thread.name.as_deref() == Some(query) || title == query {
        return Some(0);
    }
    let query = query.to_lowercase();
    let name = thread.name.as_deref().unwrap_or_default().to_lowercase();
    let title = title.to_lowercase();
    let preview = thread.preview.to_lowercase();
    if name == query || title == query {
        Some(1)
    } else if name.contains(&query) || title.contains(&query) {
        Some(2)
    } else if preview.contains(&query) {
        Some(3)
    } else {
        None
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn thread(name: Option<&str>, preview: &str) -> codex::Thread {
        serde_json::from_value(json!({
            "id": "thread-1",
            "name": name,
            "preview": preview,
            "cwd": "/tmp/project",
            "createdAt": 1,
            "updatedAt": 2,
            "recencyAt": 2,
            "source": "cli",
            "gitInfo": null
        }))
        .unwrap()
    }

    #[test]
    fn ranks_thread_matches_by_specificity() {
        let named = thread(Some("Fix popup layout"), "preview text");
        assert_eq!(thread_match_rank(&named, "Fix popup layout"), Some(0));
        assert_eq!(thread_match_rank(&named, "fix popup layout"), Some(1));
        assert_eq!(thread_match_rank(&named, "popup"), Some(2));
        assert_eq!(thread_match_rank(&named, "preview"), Some(3));
        assert_eq!(thread_match_rank(&named, "missing"), None);

        let unnamed = thread(None, "First preview line");
        assert_eq!(thread_match_rank(&unnamed, "First preview line"), Some(0));
    }
}
