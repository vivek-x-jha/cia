mod agent;
mod codex;
mod config;
mod model;
mod pi;
mod runner;
mod state;
mod tmux;
mod ui;

use std::{borrow::Cow, path::PathBuf, process::Command};

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
        /// Harness to search: pi, claude, codex, cursor, opencode, or all
        #[arg(long, default_value = "all")]
        harness: String,
    },

    /// Resume an agent thread while preserving tmux metadata
    #[command(hide = true)]
    RunThread {
        #[arg(long)]
        thread_id: Option<String>,
        #[arg(long)]
        cwd_hex: String,
        #[arg(long)]
        title_hex: String,
        #[arg(long)]
        harness_id: Option<String>,
        #[arg(long)]
        agent_command_hex: Option<String>,
        #[arg(long)]
        codex_command_hex: Option<String>,
        #[arg(long)]
        session_dir_hex: Option<String>,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    let config = config::Config::load()?;
    match args.command {
        Some(Commands::Open {
            query,
            archived,
            harness,
        }) => {
            return open_thread_by_query(&config, args.project, &query, archived, &harness);
        }
        Some(Commands::RunThread {
            thread_id,
            cwd_hex,
            title_hex,
            harness_id,
            agent_command_hex,
            codex_command_hex,
            session_dir_hex,
        }) => {
            let cwd = PathBuf::from(runner::decode(&cwd_hex, "working directory")?);
            let title = runner::decode(&title_hex, "chat title")?;
            let command_hex = agent_command_hex
                .or(codex_command_hex)
                .context("missing agent command")?;
            let agent_command = runner::decode(&command_hex, "agent command")?;
            let session_dir = session_dir_hex
                .as_deref()
                .map(|value| runner::decode(value, "session directory"))
                .transpose()?;
            return run_thread(
                &config.tmux,
                harness_id.as_deref().unwrap_or(agent::DEFAULT_HARNESS_ID),
                thread_id.as_deref(),
                &cwd,
                &title,
                &agent_command,
                session_dir.as_deref(),
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
    harness_filter: &str,
) -> Result<()> {
    let project = project
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)
        .context("failed to determine current project directory")?;
    let project = tmux::normalized_path(project)
        .to_string_lossy()
        .into_owned();
    let mut harnesses = start_harnesses(config)?;
    harnesses.retain(|harness| harness_filter == "all" || harness.id == harness_filter);
    if harnesses.is_empty() {
        anyhow::bail!("no harness matching `{harness_filter}`");
    }
    let mut project_threads = Vec::new();
    for harness in &mut harnesses {
        project_threads.extend(
            harness
                .list_threads(archived)?
                .into_iter()
                .filter(|thread| tmux::normalized_path(&thread.cwd).to_string_lossy() == project),
        );
    }
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
    let harness = harnesses
        .iter()
        .find(|harness| harness.id == thread.harness_id)
        .context("matched thread belongs to an unavailable harness")?;

    let tmux = tmux::Client::new(config.tmux.clone());
    let mut state = state::State::load(&config::state_path())?;
    let mut windows = tmux.inventory()?;
    state.reconcile(&mut windows);
    if let Some(window) = windows.iter().find(|window| {
        window.harness_id.as_deref() == Some(harness.id.as_str())
            && window.thread_id.as_deref() == Some(thread.id.as_str())
    }) {
        return tmux.switch_to(window);
    }

    let cia_command = std::env::current_exe()
        .context("failed to locate the CIA executable")?
        .to_string_lossy()
        .into_owned();
    let window = tmux.open_agent(tmux::AgentLaunch {
        inventory: &windows,
        cwd: &thread.cwd,
        title: thread.title(),
        harness_id: &harness.id,
        thread_id: Some(&thread.id),
        cia_command: &cia_command,
        agent_command: &harness.command,
        session_dir: pi_session_dir(config, &harness.id),
    })?;
    state.record(&harness.id, &thread.id, &window);
    state.last_project = Some(thread.cwd.clone());
    state.save(&config::state_path())?;
    tmux.switch_to(&window)
}

fn start_harnesses(config: &config::Config) -> Result<Vec<agent::Harness>> {
    let mut harnesses = Vec::new();
    let mut errors = Vec::new();
    for result in agent::Harness::start_all(config) {
        match result {
            Ok(harness) => harnesses.push(harness),
            Err(error) => errors.push(error.to_string()),
        }
    }
    if harnesses.is_empty() {
        anyhow::bail!("no harnesses available: {}", errors.join("; "));
    }
    Ok(harnesses)
}

fn thread_match_rank(thread: &agent::Thread, query: &str) -> Option<u8> {
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
    harness_id: &str,
    thread_id: Option<&str>,
    cwd: &PathBuf,
    title: &str,
    agent_command: &str,
    session_dir: Option<&str>,
) -> Result<()> {
    let pane_id = std::env::var_os("TMUX_PANE").map(|value| value.to_string_lossy().into_owned());
    let tmux = tmux::Client::new(tmux_config.clone());
    let kind = match harness_id {
        agent::PI_HARNESS_ID => agent::HarnessKind::Pi,
        agent::CODEX_HARNESS_ID => agent::HarnessKind::Codex,
        _ => agent::HarnessKind::Basic,
    };
    let existing_id = if kind == agent::HarnessKind::Codex && thread_id.is_none() {
        let mut client = codex::Client::start(agent_command)?;
        let cwd = tmux::normalized_path(cwd).to_string_lossy().into_owned();
        client
            .list_threads_inner(false)?
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
            tmux.mark_pane(pane_id, harness_id, thread_id, title)?;
        } else {
            tmux.mark_title(pane_id, harness_id, title)?;
        }
    }
    let cwd = cwd.to_string_lossy().into_owned();
    let args = agent_args(kind, resume_id, &cwd, title, session_dir);
    let command_line = shell_command(agent_command, &args);
    let mut command = Command::new("zsh");
    command.args(["-lc", &command_line]);
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to run {agent_command} through zsh"))?;
    if kind == agent::HarnessKind::Codex && resume_id.is_none() {
        if let Some(pane_id) = &pane_id {
            tmux.rename_active_agent_thread(pane_id, title)?;
        }
    }
    let status = child.wait()?;
    if !status.success() {
        bail!("{agent_command} exited with {status}");
    }
    Ok(())
}

fn pi_session_dir<'a>(config: &'a config::Config, harness_id: &str) -> Option<&'a str> {
    (harness_id == agent::PI_HARNESS_ID)
        .then_some(config.pi.session_dir.as_deref())
        .flatten()
}

fn agent_args(
    kind: agent::HarnessKind,
    resume_id: Option<&str>,
    cwd: &str,
    title: &str,
    session_dir: Option<&str>,
) -> Vec<String> {
    let mut args = Vec::new();
    match kind {
        agent::HarnessKind::Codex => {
            if let Some(thread_id) = resume_id {
                args.extend(["resume".into(), thread_id.into(), "-C".into(), cwd.into()]);
            } else {
                args.extend(["-C".into(), cwd.into()]);
            }
        }
        agent::HarnessKind::Pi => {
            if let Some(thread_id) = resume_id {
                args.extend(["--session".into(), thread_id.into()]);
            } else {
                args.extend(["--name".into(), title.into()]);
            }
            if let Some(session_dir) = session_dir {
                args.extend(["--session-dir".into(), session_dir.into()]);
            }
        }
        agent::HarnessKind::Basic => {}
    }
    args
}

fn shell_command(command: &str, args: &[String]) -> String {
    let command = std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .map(|value| shell_escape::unix::escape(Cow::Borrowed(value)).into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    format!("exec {command}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn thread(name: Option<&str>, preview: &str) -> agent::Thread {
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

    #[test]
    fn shell_command_execs_with_escaped_arguments() {
        let args = vec!["resume".into(), "thread; touch nope".into()];
        assert_eq!(
            shell_command("codex agent", &args),
            "exec 'codex agent' resume 'thread; touch nope'"
        );
    }

    #[test]
    fn pi_args_include_configured_session_dir() {
        assert_eq!(
            agent_args(
                agent::HarnessKind::Pi,
                Some("thread-1"),
                "/repo",
                "Ignored",
                Some("/tmp/pi sessions")
            ),
            vec!["--session", "thread-1", "--session-dir", "/tmp/pi sessions"]
        );
        assert_eq!(
            agent_args(
                agent::HarnessKind::Pi,
                None,
                "/repo",
                "Named chat",
                Some("/tmp/pi sessions")
            ),
            vec!["--name", "Named chat", "--session-dir", "/tmp/pi sessions"]
        );
    }

    #[test]
    fn launch_only_harnesses_use_configured_command_without_args() {
        assert!(agent_args(agent::HarnessKind::Basic, None, "/repo", "Chat", None).is_empty());
    }
}
