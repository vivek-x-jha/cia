use std::{
    collections::HashSet,
    env, fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
        MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, size as terminal_size, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Wrap,
    },
    Terminal,
};

use crate::{
    agent::{
        Harness, Message, Thread, CLAUDE_HARNESS_ID, CODEX_HARNESS_ID, CURSOR_HARNESS_ID,
        OPENCODE_HARNESS_ID, PI_HARNESS_ID,
    },
    config::{state_dir, state_path, Config, ThemeConfig},
    model::{build_projects, rows, Project, Row},
    state::State,
    tmux::{AgentLaunch, Client as TmuxClient, Window},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Projects,
    Threads,
    Preview,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusAction {
    Open,
    NewProject,
    New,
    Search,
    ToggleArchived,
    SetArchived,
    SetUnarchived,
    Delete,
    Help,
}

#[derive(Clone, Copy, Debug)]
struct PaneAreas {
    status: Rect,
    projects: Rect,
    threads: Rect,
    preview: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClickTarget {
    Project(usize),
    Thread(usize),
    Preview,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeleteChoice {
    No,
    Hide,
    Delete,
}

#[derive(Clone, Debug)]
struct DeletePrompt {
    target: DeleteTarget,
    choice: DeleteChoice,
}

#[derive(Clone, Debug)]
enum DeleteTarget {
    Project { name: String, cwd: String },
    Chat { thread: Thread },
}

#[derive(Clone, Copy, Debug)]
struct LastClick {
    target: ClickTarget,
    at: Instant,
}

const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(500);
const STATUS_GAP: &str = "   ";

pub struct App {
    config: Config,
    cia_command: String,
    username: String,
    harnesses: Vec<Harness>,
    tmux: TmuxClient,
    state: State,
    state_path: PathBuf,
    projects: Vec<Project>,
    all_windows: Vec<Window>,
    project_index: usize,
    row_index: usize,
    focus: Focus,
    show_archived: bool,
    search_mode: bool,
    query: String,
    new_project_mode: bool,
    new_project_path: String,
    new_chat_mode: bool,
    new_chat_picking_harness: bool,
    new_chat_harness_index: usize,
    new_chat_harness_error: Option<String>,
    new_chat_name: String,
    preview: Vec<Message>,
    preview_scroll: u16,
    status: String,
    show_help: bool,
    delete_prompt: Option<DeletePrompt>,
    last_click: Option<LastClick>,
    pending_g: bool,
    running: bool,
}

impl App {
    pub fn new(config: Config, preferred_project: Option<PathBuf>) -> Result<Self> {
        let state_path = state_path();
        let state = State::load(&state_path)
            .with_context(|| format!("failed to load {}", state_path.display()))?;
        let mut harnesses = Vec::new();
        let mut errors = Vec::new();
        for result in Harness::start_all(&config) {
            match result {
                Ok(harness) => harnesses.push(harness),
                Err(error) => errors.push(error.to_string()),
            }
        }
        if harnesses.is_empty() {
            anyhow::bail!("no harnesses available: {}", errors.join("; "));
        }
        let tmux = TmuxClient::new(config.tmux.clone());
        let cia_command = std::env::current_exe()
            .context("failed to locate the CIA executable")?
            .to_string_lossy()
            .into_owned();
        let username = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "You".into());
        let show_archived = config.ui.archived_default;
        let mut app = Self {
            config,
            cia_command,
            username,
            harnesses,
            tmux,
            state,
            state_path,
            projects: Vec::new(),
            all_windows: Vec::new(),
            project_index: 0,
            row_index: 0,
            focus: Focus::Projects,
            show_archived,
            search_mode: false,
            query: String::new(),
            new_project_mode: false,
            new_project_path: String::new(),
            new_chat_mode: false,
            new_chat_picking_harness: false,
            new_chat_harness_index: 0,
            new_chat_harness_error: None,
            new_chat_name: String::new(),
            preview: Vec::new(),
            preview_scroll: 0,
            status: String::new(),
            show_help: false,
            delete_prompt: None,
            last_click: None,
            pending_g: false,
            running: true,
        };
        app.refresh()?;
        if let Some(path) = preferred_project {
            let path = crate::tmux::normalized_path(path)
                .to_string_lossy()
                .into_owned();
            if let Some(index) = app.projects.iter().position(|project| project.cwd == path) {
                app.project_index = index;
            }
        } else if let Some(last) = &app.state.last_project {
            if let Some(index) = app.projects.iter().position(|project| &project.cwd == last) {
                app.project_index = index;
            }
        }
        app.load_preview();
        Ok(app)
    }

    fn refresh(&mut self) -> Result<()> {
        let mut active_threads = Vec::new();
        let mut archived_threads = Vec::new();
        let mut errors = Vec::new();
        for harness in &mut self.harnesses {
            match harness.list_threads(false) {
                Ok(mut harness_threads) => active_threads.append(&mut harness_threads),
                Err(error) => errors.push(format!("{}: {error}", harness.label)),
            }
            match harness.list_threads(true) {
                Ok(mut harness_threads) => archived_threads.append(&mut harness_threads),
                Err(error) => errors.push(format!("{} archived: {error}", harness.label)),
            }
        }
        if active_threads.is_empty() && archived_threads.is_empty() && !errors.is_empty() {
            anyhow::bail!("{}", errors.join("; "));
        }
        let mut all_threads = Vec::new();
        let mut seen_threads = HashSet::new();
        for mut thread in active_threads.into_iter().chain(archived_threads) {
            if !seen_threads.insert((thread.harness_id.clone(), thread.id.clone())) {
                continue;
            }
            thread.archived = self.state.is_archived(&thread.harness_id, &thread.id);
            all_threads.push(thread);
        }
        let threads: Vec<Thread> = all_threads
            .iter()
            .filter(|thread| self.show_archived || !thread.archived)
            .cloned()
            .collect();
        let archived_thread_ids: HashSet<(String, String)> = self
            .state
            .archived_threads
            .iter()
            .map(|thread| (thread.harness_id.clone(), thread.thread_id.clone()))
            .collect();
        let archived_thread_names: HashSet<(String, String, String)> = all_threads
            .iter()
            .filter(|thread| thread.archived)
            .filter_map(|thread| {
                thread.name.as_ref().map(|name| {
                    (
                        thread.harness_id.clone(),
                        name.clone(),
                        crate::tmux::normalized_path(&thread.cwd)
                            .to_string_lossy()
                            .into_owned(),
                    )
                })
            })
            .collect();
        let mut windows = self.tmux.inventory()?;
        self.state.reconcile(&mut windows);
        self.all_windows = windows.clone();
        if self.show_archived {
            windows.retain(|window| window.thread_id.is_some() || window.chat_title.is_some());
        } else {
            windows.retain(|window| {
                !window_matches_archived_thread(
                    window,
                    &archived_thread_ids,
                    &archived_thread_names,
                )
            });
        }
        self.projects = build_projects(threads, windows, &self.tmux);
        self.projects
            .retain(|project| !self.state.is_project_hidden(&project.cwd));
        for cwd in &self.state.project_paths {
            if !self.projects.iter().any(|project| &project.cwd == cwd) {
                self.projects.push(Project {
                    name: project_name(cwd),
                    cwd: cwd.clone(),
                    recency: 0,
                    threads: Vec::new(),
                    agents: Vec::new(),
                });
            }
        }
        self.projects
            .sort_by(|a, b| b.recency.cmp(&a.recency).then_with(|| a.name.cmp(&b.name)));
        self.project_index = self
            .project_index
            .min(self.projects.len().saturating_sub(1));
        self.row_index = self
            .row_index
            .min(self.current_rows().len().saturating_sub(1));
        self.state.save(&self.state_path)?;
        self.status = format!(
            "{} projects · {} threads",
            self.projects.len(),
            self.projects
                .iter()
                .map(|project| project.threads.len())
                .sum::<usize>()
        );
        if !errors.is_empty() {
            self.status = errors.join("; ");
        }
        Ok(())
    }

    fn harness(&self, harness_id: &str) -> Option<&Harness> {
        self.harnesses
            .iter()
            .find(|harness| harness.id == harness_id)
    }

    fn harness_mut(&mut self, harness_id: &str) -> Option<&mut Harness> {
        self.harnesses
            .iter_mut()
            .find(|harness| harness.id == harness_id)
    }

    fn current_project(&self) -> Option<&Project> {
        self.projects.get(self.project_index)
    }

    fn visible_project_indices(&self) -> Vec<usize> {
        let query = self.query.to_lowercase();
        self.projects
            .iter()
            .enumerate()
            .filter(|(_, project)| {
                query.is_empty()
                    || fuzzy_matches(&project.name, &query)
                    || fuzzy_matches(&project.cwd, &query)
                    || rows(project)
                        .iter()
                        .any(|row| self.row_matches_query(row, &query))
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn current_rows(&self) -> Vec<Row> {
        let query = self.query.to_lowercase();
        let project_matches = self.current_project().is_some_and(|project| {
            fuzzy_matches(&project.name, &query) || fuzzy_matches(&project.cwd, &query)
        });
        self.current_project()
            .map(rows)
            .unwrap_or_default()
            .into_iter()
            .filter(|row| {
                query.is_empty() || project_matches || self.row_matches_query(row, &query)
            })
            .collect()
    }

    fn row_matches_query(&self, row: &Row, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        if fuzzy_matches(&row.title(), query) {
            return true;
        }
        if let Row::Thread { thread, .. } = row {
            if contains_ignore_case(&thread.preview, query) {
                return true;
            }
        }
        row_harness_id(row).is_some_and(|harness_id| {
            fuzzy_matches(harness_id, query)
                || self.harness(harness_id).is_some_and(|harness| {
                    fuzzy_matches(&harness.label, query) || fuzzy_matches(&harness.marker, query)
                })
        })
    }

    fn reset_search_selection(&mut self) {
        self.project_index = self.visible_project_indices().first().copied().unwrap_or(0);
        self.row_index = 0;
        self.load_preview();
    }

    fn load_preview(&mut self) {
        self.preview.clear();
        self.preview_scroll = 0;
        let row = self.current_rows().get(self.row_index).cloned();
        if let Some(Row::Thread { thread, .. }) = row {
            let turns = self.config.codex.transcript_turns;
            match self
                .harness_mut(&thread.harness_id)
                .map(|harness| harness.read_messages(&thread.id, turns))
                .transpose()
            {
                Ok(messages) => {
                    self.preview = messages.unwrap_or_default();
                }
                Err(error) => self.status = error.to_string(),
            }
        }
    }

    fn select_next(&mut self, delta: isize) {
        match self.focus {
            Focus::Projects => {
                let visible = self.visible_project_indices();
                let position = visible
                    .iter()
                    .position(|index| *index == self.project_index)
                    .unwrap_or(0);
                self.project_index = visible
                    .get(move_index(position, visible.len(), delta))
                    .copied()
                    .unwrap_or(0);
                self.row_index = 0;
            }
            Focus::Threads => {
                self.row_index = move_index(self.row_index, self.current_rows().len(), delta);
            }
            Focus::Preview => {}
        }
        self.load_preview();
    }

    fn scroll_preview(&mut self, delta: isize) {
        self.preview_scroll = if delta.is_negative() {
            self.preview_scroll
                .saturating_add(delta.unsigned_abs() as u16)
        } else {
            self.preview_scroll.saturating_sub(delta as u16)
        };
        self.focus = Focus::Preview;
    }

    fn select_boundary(&mut self, last: bool) {
        match self.focus {
            Focus::Projects => {
                let visible = self.visible_project_indices();
                self.project_index = if last {
                    visible.last().copied().unwrap_or(0)
                } else {
                    visible.first().copied().unwrap_or(0)
                };
                self.row_index = 0;
            }
            Focus::Threads => {
                self.row_index = if last {
                    self.current_rows().len().saturating_sub(1)
                } else {
                    0
                };
            }
            Focus::Preview => {}
        }
        self.load_preview();
    }

    fn activate(&mut self) {
        let Some(row) = self.current_rows().get(self.row_index).cloned() else {
            return;
        };
        let result = match row {
            Row::Agent(window) => self.tmux.switch_to(&window),
            Row::Thread {
                live: Some(window), ..
            } => self.tmux.switch_to(&window),
            Row::Thread { thread, live: None } => self.open_thread(&thread),
        };
        match result {
            Ok(()) => self.running = false,
            Err(error) => self.status = error.to_string(),
        }
    }

    fn open_thread(&mut self, thread: &Thread) -> Result<()> {
        let (harness_id, harness_command) = self
            .harness(&thread.harness_id)
            .map(|harness| {
                if harness.command_available() {
                    Ok((harness.id.clone(), harness.command.clone()))
                } else {
                    Err(anyhow::anyhow!(harness.missing_cli_message()))
                }
            })
            .context("thread belongs to an unavailable harness")??;
        let window = self.tmux.open_agent(AgentLaunch {
            inventory: &self.all_windows,
            cwd: &thread.cwd,
            title: thread.title(),
            harness_id: &harness_id,
            thread_id: Some(&thread.id),
            cia_command: &self.cia_command,
            agent_command: &harness_command,
            session_dir: self.pi_session_dir(&harness_id),
        })?;
        self.state.record(&harness_id, &thread.id, &window);
        self.state.last_project = Some(thread.cwd.clone());
        self.state.save(&self.state_path)?;
        self.tmux.switch_to(&window)
    }

    fn begin_new_project(&mut self) {
        self.new_project_path.clear();
        self.new_project_mode = true;
    }

    fn begin_new_thread(&mut self) {
        self.new_chat_name.clear();
        self.new_chat_harness_error = None;
        self.new_chat_harness_index = self
            .harnesses
            .iter()
            .position(|harness| harness.id == self.config.ui.default_harness)
            .unwrap_or(0);
        self.new_chat_picking_harness = self.harnesses.len() > 1;
        self.new_chat_mode = true;
    }

    fn toggle_archived(&mut self) {
        self.show_archived = !self.show_archived;
        if let Err(error) = self.refresh() {
            self.status = error.to_string();
        }
        self.load_preview();
    }

    fn select_new_chat_harness(&mut self) {
        let Some(harness) = self.harnesses.get(self.new_chat_harness_index) else {
            self.new_chat_harness_error = Some("Selected harness is unavailable".into());
            return;
        };
        if harness.command_available() {
            self.new_chat_harness_error = None;
            self.new_chat_picking_harness = false;
        } else {
            self.new_chat_harness_error = Some(harness.missing_cli_message());
        }
    }

    fn move_new_chat_harness(&mut self, delta: isize) {
        self.new_chat_harness_index =
            move_index(self.new_chat_harness_index, self.harnesses.len(), delta);
        self.new_chat_harness_error = None;
    }

    fn set_new_chat_harness_index(&mut self, index: usize) {
        self.new_chat_harness_index = index;
        self.select_new_chat_harness();
    }

    fn set_selected_archived(&mut self, archived: bool) {
        let Some(Row::Thread { thread, .. }) = self.current_rows().get(self.row_index).cloned()
        else {
            self.status = "Select a saved chat first".into();
            return;
        };
        self.state
            .set_archived(&thread.harness_id, &thread.id, archived);
        if let Err(error) = self.state.save(&self.state_path) {
            self.status = error.to_string();
            return;
        }
        self.refresh_view();
    }

    fn begin_delete(&mut self) {
        let target = match self.focus {
            Focus::Projects => {
                let Some(project) = self.current_project() else {
                    self.status = "Select a project first".into();
                    return;
                };
                DeleteTarget::Project {
                    name: project.name.clone(),
                    cwd: project.cwd.clone(),
                }
            }
            Focus::Threads | Focus::Preview => {
                let Some(Row::Thread { thread, .. }) =
                    self.current_rows().get(self.row_index).cloned()
                else {
                    self.status = "Select a saved chat first".into();
                    return;
                };
                DeleteTarget::Chat { thread: *thread }
            }
        };
        self.delete_prompt = Some(DeletePrompt {
            target,
            choice: DeleteChoice::No,
        });
    }

    fn confirm_delete(&mut self) {
        let Some(prompt) = self.delete_prompt.take() else {
            return;
        };
        let result = match (prompt.target, prompt.choice) {
            (_, DeleteChoice::No) => Ok(()),
            (DeleteTarget::Project { cwd, .. }, DeleteChoice::Hide) => self.hide_project(&cwd),
            (DeleteTarget::Project { cwd, .. }, DeleteChoice::Delete) => {
                self.delete_project_from_disk(&cwd)
            }
            (DeleteTarget::Chat { thread }, DeleteChoice::Delete) => self.delete_chat(&thread),
            (DeleteTarget::Chat { .. }, DeleteChoice::Hide) => Ok(()),
        };
        match result {
            Ok(()) => {
                self.refresh_view();
            }
            Err(error) => self.status = error.to_string(),
        }
    }

    fn hide_project(&mut self, cwd: &str) -> Result<()> {
        self.state.hide_project_path(cwd);
        self.state.save(&self.state_path)
    }

    fn delete_project_from_disk(&mut self, cwd: &str) -> Result<()> {
        remove_path_from_disk(Path::new(cwd))?;
        self.state.hide_project_path(cwd);
        self.state.save(&self.state_path)
    }

    fn delete_chat(&mut self, thread: &Thread) -> Result<()> {
        self.delete_chat_files(thread)?;
        self.kill_deleted_chat_agent_panes(thread)?;
        self.state.save(&self.state_path)
    }

    fn kill_deleted_chat_agent_panes(&mut self, thread: &Thread) -> Result<()> {
        let windows = self.tmux.inventory()?;
        for window in windows.iter().filter(|window| {
            deleted_chat_agent_pane_matches(window, thread, &self.config.tmux.agent_window_names)
        }) {
            self.tmux.kill_pane(&window.pane_id)?;
        }
        Ok(())
    }

    fn delete_chat_files(&mut self, thread: &Thread) -> Result<()> {
        let mut removed = false;
        for path in thread.storage_paths() {
            remove_path_from_disk(Path::new(path))?;
            removed = true;
        }
        if !removed {
            anyhow::bail!(
                "No on-disk chat path is known for this {} chat",
                thread.harness_id
            );
        }
        self.state
            .set_archived(&thread.harness_id, &thread.id, false);
        Ok(())
    }

    fn refresh_view(&mut self) {
        if let Err(error) = self.refresh() {
            self.status = error.to_string();
        }
        self.load_preview();
    }

    fn run_status_action(&mut self, action: StatusAction) {
        match action {
            StatusAction::Open => self.activate(),
            StatusAction::NewProject => self.begin_new_project(),
            StatusAction::New => self.begin_new_thread(),
            StatusAction::Search => self.search_mode = true,
            StatusAction::ToggleArchived => self.toggle_archived(),
            StatusAction::SetArchived => self.set_selected_archived(true),
            StatusAction::SetUnarchived => self.set_selected_archived(false),
            StatusAction::Delete => self.begin_delete(),
            StatusAction::Help => self.show_help = true,
        }
    }

    fn submit_new_project(&mut self) {
        let path = self.new_project_path.trim();
        if path.is_empty() {
            self.status = "Project path cannot be empty".into();
            return;
        }
        let cwd = new_project_path(path);
        if let Err(error) = fs::create_dir_all(&cwd) {
            self.status = format!("failed to create {}: {error}", cwd.display());
            return;
        }
        let cwd = crate::tmux::normalized_path(&cwd)
            .to_string_lossy()
            .into_owned();
        self.state.add_project_path(cwd.clone());
        self.state.last_project = Some(cwd.clone());
        if let Err(error) = self.state.save(&self.state_path) {
            self.status = error.to_string();
            return;
        }
        self.new_project_mode = false;
        self.refresh_view();
        if let Some(index) = self.projects.iter().position(|project| project.cwd == cwd) {
            self.project_index = index;
            self.row_index = 0;
            self.focus = Focus::Projects;
            self.load_preview();
        }
    }

    fn submit_new_thread(&mut self) {
        let Some(project) = self.current_project().cloned() else {
            return;
        };
        let title = self.new_chat_name.trim();
        if title.is_empty() {
            self.status = "Chat name cannot be empty".into();
            return;
        }
        let Some(harness) = self.harnesses.get(self.new_chat_harness_index) else {
            self.status = "Selected harness is unavailable".into();
            return;
        };
        if !harness.command_available() {
            self.new_chat_harness_error = Some(harness.missing_cli_message());
            self.new_chat_picking_harness = true;
            return;
        }
        let (harness_id, harness_command) = (harness.id.clone(), harness.command.clone());
        if project
            .threads
            .iter()
            .any(|thread| thread.harness_id == harness_id && thread.name.as_deref() == Some(title))
        {
            self.status = format!("A chat named `{title}` already exists in this project");
            return;
        }
        let result = self
            .tmux
            .open_agent(AgentLaunch {
                inventory: &self.all_windows,
                cwd: &project.cwd,
                title,
                harness_id: &harness_id,
                thread_id: None,
                cia_command: &self.cia_command,
                agent_command: &harness_command,
                session_dir: self.pi_session_dir(&harness_id),
            })
            .and_then(|window| {
                self.state.last_project = Some(project.cwd.clone());
                self.state.save(&self.state_path)?;
                self.tmux.switch_to(&window)
            });
        match result {
            Ok(()) => {
                self.new_chat_mode = false;
                self.running = false;
            }
            Err(error) => self.status = error.to_string(),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if self.delete_prompt.is_some() {
            match key.code {
                KeyCode::Esc => self.delete_prompt = None,
                KeyCode::Enter => self.confirm_delete(),
                KeyCode::Left | KeyCode::Char('h') => {
                    if let Some(prompt) = &mut self.delete_prompt {
                        prompt.choice = previous_delete_choice(prompt.choice, &prompt.target);
                    }
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    if let Some(prompt) = &mut self.delete_prompt {
                        prompt.choice = next_delete_choice(prompt.choice, &prompt.target);
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') => self.delete_prompt = None,
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if let Some(prompt) = &mut self.delete_prompt {
                        prompt.choice = default_delete_choice(&prompt.target);
                    }
                    self.confirm_delete();
                }
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    if let Some(prompt) = &mut self.delete_prompt {
                        prompt.choice = DeleteChoice::Delete;
                    }
                    self.confirm_delete();
                }
                _ => {}
            }
            return;
        }
        if self.new_project_mode {
            match key.code {
                KeyCode::Esc => {
                    self.new_project_mode = false;
                    self.new_project_path.clear();
                }
                KeyCode::Enter => self.submit_new_project(),
                KeyCode::Backspace => {
                    self.new_project_path.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.new_project_path.clear();
                }
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && self.new_project_path.chars().count() < 240 =>
                {
                    self.new_project_path.push(character);
                }
                _ => {}
            }
            return;
        }
        if self.new_chat_mode {
            if self.new_chat_picking_harness {
                match key.code {
                    KeyCode::Esc => {
                        self.new_chat_mode = false;
                        self.new_chat_picking_harness = false;
                        self.new_chat_harness_error = None;
                        self.new_chat_name.clear();
                    }
                    KeyCode::Enter => self.select_new_chat_harness(),
                    KeyCode::Left | KeyCode::Char('h') | KeyCode::Up | KeyCode::Char('k') => {
                        self.move_new_chat_harness(-1);
                    }
                    KeyCode::Right | KeyCode::Char('l') | KeyCode::Down | KeyCode::Char('j') => {
                        self.move_new_chat_harness(1);
                    }
                    KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.move_new_chat_harness(1);
                    }
                    KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.move_new_chat_harness(-1);
                    }
                    _ => {}
                }
                return;
            }
            match key.code {
                KeyCode::Esc => {
                    self.new_chat_mode = false;
                    self.new_chat_picking_harness = false;
                    self.new_chat_harness_error = None;
                    self.new_chat_name.clear();
                }
                KeyCode::Enter => self.submit_new_thread(),
                KeyCode::Backspace => {
                    self.new_chat_name.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.new_chat_name.clear();
                }
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && self.new_chat_name.chars().count() < 80 =>
                {
                    self.new_chat_name.push(character);
                }
                _ => {}
            }
            return;
        }
        if self.search_mode {
            match key.code {
                KeyCode::Esc => {
                    self.search_mode = false;
                    self.query.clear();
                    self.reset_search_selection();
                }
                KeyCode::Enter => self.search_mode = false,
                KeyCode::Backspace => {
                    self.query.pop();
                    self.reset_search_selection();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.query.clear();
                    self.reset_search_selection();
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.query.push(character);
                    self.reset_search_selection();
                }
                _ => {}
            }
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        if key.code == KeyCode::Char('g') && key.modifiers == KeyModifiers::NONE {
            if self.pending_g {
                self.pending_g = false;
                self.select_boundary(false);
            } else {
                self.pending_g = true;
            }
            return;
        }
        self.pending_g = false;
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.running = false,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('/') => self.search_mode = true,
            KeyCode::Char('a') => self.toggle_archived(),
            KeyCode::Char('A') => self.set_selected_archived(true),
            KeyCode::Char('U') => self.set_selected_archived(false),
            KeyCode::Char('D') => self.begin_delete(),
            KeyCode::Char('r') => self.refresh_view(),
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_next(1)
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_next(-1)
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_preview(8)
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_preview(-8)
            }
            KeyCode::Char('N') => self.begin_new_project(),
            KeyCode::Char('n') => self.begin_new_thread(),
            KeyCode::Char('G') => self.select_boundary(true),
            KeyCode::Enter => self.activate(),
            KeyCode::Down | KeyCode::Char('j') => self.select_next(1),
            KeyCode::Up | KeyCode::Char('k') => self.select_next(-1),
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.focus = next_focus(self.focus)
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.focus = previous_focus(self.focus)
            }
            _ => {}
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let Ok((width, height)) = terminal_size() else {
            return;
        };
        let areas = pane_areas(Rect::new(0, 0, width, height));
        match mouse.kind {
            MouseEventKind::ScrollDown => self.scroll_preview(3),
            MouseEventKind::ScrollUp => self.scroll_preview(-3),
            MouseEventKind::Down(_) => self.handle_click(mouse.column, mouse.row, areas),
            _ => {}
        }
    }

    fn handle_click(&mut self, x: u16, y: u16, areas: PaneAreas) {
        if self.delete_prompt.is_some() {
            self.handle_delete_prompt_click(x, y);
            return;
        }
        if self.new_project_mode {
            self.last_click = None;
            return;
        }
        if self.new_chat_mode {
            self.handle_new_chat_click(x, y);
            return;
        }
        if self.show_help {
            self.show_help = false;
            self.last_click = None;
            return;
        }
        if contains(areas.status, x, y) {
            if let Some(action) =
                status_action_at(self, x.saturating_sub(areas.status.x), areas.status.width)
            {
                self.run_status_action(action);
            }
            self.last_click = None;
            return;
        }
        if contains(areas.projects, x, y) {
            self.focus = Focus::Projects;
            if let Some(index) = list_index_at(y, areas.projects) {
                if let Some(project_index) = self.visible_project_indices().get(index).copied() {
                    let double_click = self.register_click(ClickTarget::Project(project_index));
                    if project_index != self.project_index {
                        self.project_index = project_index;
                        self.row_index = 0;
                        self.load_preview();
                    }
                    if double_click {
                        self.focus = Focus::Threads;
                    }
                }
            }
            return;
        }
        if contains(areas.threads, x, y) {
            self.focus = Focus::Threads;
            if let Some(index) = list_index_at(y, areas.threads) {
                if index < self.current_rows().len() {
                    let double_click = self.register_click(ClickTarget::Thread(index));
                    if index != self.row_index {
                        self.row_index = index;
                        self.load_preview();
                    }
                    if double_click {
                        self.activate();
                    }
                }
            }
            return;
        }
        if contains(areas.preview, x, y) {
            self.focus = Focus::Preview;
            self.register_click(ClickTarget::Preview);
        }
    }

    fn handle_delete_prompt_click(&mut self, x: u16, y: u16) {
        let Ok((width, height)) = terminal_size() else {
            return;
        };
        let popup = centered(Rect::new(0, 0, width, height), 76, 8);
        let Some(prompt) = &self.delete_prompt else {
            return;
        };
        match delete_choice_at(x, y, popup, &prompt.target) {
            Some(DeleteChoice::No) => self.delete_prompt = None,
            Some(choice) => {
                if let Some(prompt) = &mut self.delete_prompt {
                    prompt.choice = choice;
                }
                self.confirm_delete();
            }
            None => {}
        }
        self.last_click = None;
    }

    fn handle_new_chat_click(&mut self, x: u16, y: u16) {
        let Ok((width, height)) = terminal_size() else {
            return;
        };
        let popup = if self.new_chat_picking_harness {
            centered(
                Rect::new(0, 0, width, height),
                96,
                self.harnesses.len() as u16 + 4,
            )
        } else {
            centered(Rect::new(0, 0, width, height), 84, 5)
        };
        if !contains(popup, x, y) {
            return;
        }
        if self.new_chat_picking_harness {
            if let Some(index) = harness_index_at(x, y, popup, &self.harnesses) {
                self.set_new_chat_harness_index(index);
            }
        }
    }

    fn register_click(&mut self, target: ClickTarget) -> bool {
        let now = Instant::now();
        let double_click = self.last_click.is_some_and(|last| {
            last.target == target && now.duration_since(last.at) <= DOUBLE_CLICK_WINDOW
        });
        self.last_click = Some(LastClick { target, at: now });
        double_click
    }

    fn pi_session_dir(&self, harness_id: &str) -> Option<&str> {
        (harness_id == PI_HARNESS_ID)
            .then_some(self.config.pi.session_dir.as_deref())
            .flatten()
    }
}

pub fn run(mut app: App) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = (|| -> Result<()> {
        while app.running {
            terminal.draw(|frame| draw(frame, &app))?;
            if event::poll(Duration::from_millis(250))? {
                match event::read()? {
                    Event::Key(key) => app.handle_key(key),
                    Event::Mouse(mouse) => app.handle_mouse(mouse),
                    _ => {}
                }
            }
        }
        Ok(())
    })();
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    result
}

fn draw(frame: &mut ratatui::Frame, app: &App) {
    let theme = ResolvedTheme::from(&app.config.theme);
    let area = frame.area();
    let areas = pane_areas(area);
    draw_status_bar(frame, areas.status, app, theme);
    draw_projects(frame, areas.projects, app, theme);
    draw_threads(frame, areas.threads, app, theme);
    draw_preview(frame, areas.preview, app, theme);
    if app.show_help {
        draw_help(frame, area, theme);
    }
    if app.new_project_mode {
        draw_new_project_prompt(frame, area, app, theme);
    }
    if app.new_chat_mode {
        draw_new_chat_prompt(frame, area, app, theme);
    }
    if let Some(prompt) = &app.delete_prompt {
        draw_delete_prompt(frame, area, prompt, theme);
    }
}

fn pane_areas(area: Rect) -> PaneAreas {
    let outer = Layout::vertical([Constraint::Length(1), Constraint::Min(5)]).split(area);
    let panes = if area.width >= 100 {
        let rows = Layout::vertical([Constraint::Percentage(36), Constraint::Percentage(64)])
            .split(outer[1]);
        let top = Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)])
            .split(rows[0]);
        [top[0], top[1], rows[1]]
    } else {
        let panes = Layout::vertical([
            Constraint::Percentage(25),
            Constraint::Percentage(35),
            Constraint::Percentage(40),
        ])
        .split(outer[1]);
        [panes[0], panes[1], panes[2]]
    };
    PaneAreas {
        status: outer[0],
        projects: panes[0],
        threads: panes[1],
        preview: panes[2],
    }
}

fn draw_status_bar(frame: &mut ratatui::Frame, area: Rect, app: &App, theme: ResolvedTheme) {
    let project_count = app.projects.len();
    let thread_count = app
        .projects
        .iter()
        .map(|project| project.threads.len())
        .sum::<usize>();
    let count_status = format!("{project_count} projects · {thread_count} threads");
    let mut left_spans = vec![
        Span::styled(" ", Style::default().fg(theme.muted)),
        Span::styled(
            format!(" {project_count}"),
            Style::default().fg(theme.status_projects),
        ),
        Span::styled(STATUS_GAP, Style::default().fg(theme.muted)),
        Span::styled(
            format!("󰻞 {thread_count}"),
            Style::default().fg(theme.status_threads),
        ),
    ];
    if app.status != count_status {
        left_spans.push(Span::styled(STATUS_GAP, Style::default().fg(theme.muted)));
        left_spans.push(Span::styled(
            app.status.clone(),
            Style::default().fg(theme.error),
        ));
    }
    for (label, action) in status_actions_left(app) {
        left_spans.push(Span::styled(STATUS_GAP, Style::default().fg(theme.muted)));
        left_spans.push(Span::styled(
            label,
            Style::default().fg(status_color(action, theme)),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(left_spans)), area);

    let mut right_spans = Vec::new();
    for (index, (label, action)) in status_actions_right(app).into_iter().enumerate() {
        if index > 0 {
            right_spans.push(Span::styled(STATUS_GAP, Style::default().fg(theme.muted)));
        }
        right_spans.push(Span::styled(
            label,
            Style::default().fg(status_color(action, theme)),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(right_spans)).alignment(Alignment::Right),
        area,
    );
}

fn draw_projects(frame: &mut ratatui::Frame, area: Rect, app: &App, theme: ResolvedTheme) {
    let visible = app.visible_project_indices();
    let items: Vec<ListItem> = visible
        .iter()
        .filter_map(|index| app.projects.get(*index))
        .map(|project| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    "● ",
                    Style::default().fg(if project.agents.is_empty() {
                        theme.muted
                    } else {
                        theme.success
                    }),
                ),
                Span::raw(&project.name),
            ]))
        })
        .collect();
    let selected_index = visible
        .iter()
        .position(|index| *index == app.project_index)
        .unwrap_or(0);
    let mut state = ListState::default().with_selected(Some(selected_index));
    frame.render_stateful_widget(
        List::new(items)
            .block(panel(" Projects ", app.focus == Focus::Projects, theme))
            .highlight_style(selected(theme)),
        area,
        &mut state,
    );
}

fn draw_threads(frame: &mut ratatui::Frame, area: Rect, app: &App, theme: ResolvedTheme) {
    let items: Vec<ListItem> = app
        .current_rows()
        .iter()
        .map(|row| {
            let marker = if row.is_live() {
                Span::styled("● ", Style::default().fg(theme.success))
            } else {
                Span::styled("  ", Style::default().fg(theme.muted))
            };
            let harness_id = row_harness_id(row);
            let harness = harness_id
                .and_then(|harness_id| app.harness(harness_id))
                .map(|harness| harness.marker.as_str())
                .unwrap_or("?");
            let harness_color = harness_id
                .map(|harness_id| new_chat_harness_color(harness_id, theme))
                .unwrap_or(theme.muted);
            let archive_marker = match row {
                Row::Thread { thread, .. } if app.show_archived && thread.archived => Span::styled(
                    format!(" {}", app.config.ui.archive_icon),
                    Style::default().fg(theme.archive_icon),
                ),
                _ => Span::raw(""),
            };
            ListItem::new(Line::from(vec![
                marker,
                Span::styled(format!("{harness} "), Style::default().fg(harness_color)),
                Span::raw(row.title()),
                archive_marker,
            ]))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.row_index));
    let title = if app.show_archived {
        " All chats "
    } else {
        " Chats "
    };
    frame.render_stateful_widget(
        List::new(items)
            .block(panel(title, app.focus == Focus::Threads, theme))
            .highlight_style(selected(theme)),
        area,
        &mut state,
    );
}

fn draw_preview(frame: &mut ratatui::Frame, area: Rect, app: &App, theme: ResolvedTheme) {
    let title = " Preview ";
    let block = panel(title, app.focus == Focus::Preview, theme);
    let viewport_height = area
        .inner(Margin {
            vertical: 1,
            horizontal: 1,
        })
        .height as usize;
    let text = preview_text(app, theme);
    let content_height = text.lines.len();
    let max_scroll = content_height.saturating_sub(viewport_height);
    let scroll = max_scroll.saturating_sub(app.preview_scroll as usize);
    frame.render_widget(
        Paragraph::new(text)
            .block(block)
            .scroll((scroll as u16, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
    if max_scroll > 0 {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_symbol("│")
            .thumb_style(Style::default().fg(theme.muted));
        let mut state = ScrollbarState::new(content_height)
            .position(scroll)
            .viewport_content_length(viewport_height);
        frame.render_stateful_widget(
            scrollbar,
            area.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut state,
        );
    }
}

fn preview_text(app: &App, theme: ResolvedTheme) -> Text<'static> {
    let rows = app.current_rows();
    let mut text = Text::default();
    if let Some(row) = rows.get(app.row_index) {
        match row {
            Row::Agent(window) => {
                text.push_line(Line::styled(
                    "Unmapped live agent",
                    Style::default()
                        .fg(theme.warning)
                        .add_modifier(Modifier::BOLD),
                ));
                text.push_line(format!(
                    "{}:{} · {}",
                    window.session, window.window_name, window.cwd
                ));
                text.push_line("");
                text.push_line("CIA will switch to this window without guessing which saved thread it contains.");
            }
            Row::Thread { thread, live } => {
                let harness = app.harness(&thread.harness_id);
                let harness_label = harness
                    .map(|harness| harness.label.as_str())
                    .unwrap_or(thread.harness_id.as_str());
                text.push_line(Line::styled(
                    thread.title().to_string(),
                    Style::default()
                        .fg(theme.preview_title)
                        .add_modifier(Modifier::BOLD),
                ));
                let branch = thread
                    .git_info
                    .as_ref()
                    .and_then(|git| git.branch.as_deref())
                    .unwrap_or("no branch");
                text.push_line(Line::styled(
                    format!(
                        "{} · {} · {} · created {} · {}",
                        harness_label,
                        thread.source_label(),
                        branch,
                        format_timestamp(thread.created_at),
                        thread.cwd
                    ),
                    Style::default().fg(theme.muted),
                ));
                if live.is_some() {
                    text.push_line(Line::styled("● live", Style::default().fg(theme.success)));
                }
                text.push_line("");
                if app.preview.is_empty() {
                    let preview = if thread.preview.is_empty() {
                        "No transcript preview available.".to_string()
                    } else {
                        thread.preview.clone()
                    };
                    text.push_line(Line::styled(
                        preview,
                        Style::default().fg(theme.preview_text),
                    ));
                } else {
                    for message in &app.preview {
                        let is_user = message.role == "You";
                        let role = if is_user {
                            app.username.as_str()
                        } else {
                            harness
                                .map(|harness| harness.marker.as_str())
                                .unwrap_or(message.role.as_str())
                        };
                        text.push_line(Line::styled(
                            role.to_string(),
                            Style::default()
                                .fg(if is_user {
                                    theme.preview_user
                                } else {
                                    new_chat_harness_color(&thread.harness_id, theme)
                                })
                                .add_modifier(Modifier::BOLD),
                        ));
                        text.push_line(Line::styled(
                            message.text.clone(),
                            Style::default().fg(theme.preview_text),
                        ));
                        text.push_line("");
                    }
                }
            }
        }
    }
    text
}

fn draw_help(frame: &mut ratatui::Frame, area: Rect, theme: ResolvedTheme) {
    let popup = centered(area, 74, 18);
    frame.render_widget(Clear, popup);
    frame.render_widget(panel(" CIA Help ", true, theme), popup);

    let inner = popup.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(8),
        Constraint::Length(1),
    ])
    .split(inner);
    let columns =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(rows[1]);

    frame.render_widget(
        Paragraph::new(Text::from(vec![
            Line::styled(
                "Keyboard-first controls. Press any key, Esc, or click outside to close.",
                Style::default().fg(theme.muted),
            ),
            Line::from(""),
        ])),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(help_section(
            "Navigate",
            &[
                ("Tab / h / l", "change pane"),
                ("j / ↓", "move down"),
                ("k / ↑", "move up"),
                ("Ctrl+n / Ctrl+p", "next / previous"),
                ("gg / G", "first / last"),
                ("Ctrl+d / Ctrl+u", "scroll preview"),
            ],
            theme,
        )),
        columns[0],
    );
    frame.render_widget(
        Paragraph::new(help_section(
            "Act",
            &[
                ("Enter", "open or resume"),
                ("N / n", "new project / chat"),
                ("/", "search"),
                ("a", "active / all"),
                ("A / U", "archive / unarchive"),
                ("D", "delete or hide"),
                ("r", "refresh"),
                ("q / Esc", "quit"),
            ],
            theme,
        )),
        columns[1],
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            "Tip: the top bar has clickable shortcuts for the same actions.",
            Style::default().fg(theme.muted),
        )),
        rows[2],
    );
}

fn help_section(
    title: &'static str,
    rows: &[(&'static str, &'static str)],
    theme: ResolvedTheme,
) -> Text<'static> {
    let mut lines = vec![
        Line::styled(
            title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Line::from(""),
    ];
    lines.extend(rows.iter().map(|(keys, description)| {
        Line::from(vec![
            Span::styled(
                format!("{keys:<17}"),
                Style::default()
                    .fg(theme.status_help)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(*description, Style::default().fg(theme.foreground)),
        ])
    }));
    Text::from(lines)
}

fn draw_delete_prompt(
    frame: &mut ratatui::Frame,
    area: Rect,
    prompt: &DeletePrompt,
    theme: ResolvedTheme,
) {
    let popup = centered(area, 76, 8);
    frame.render_widget(Clear, popup);
    let (title, warning) = match &prompt.target {
        DeleteTarget::Project { name, cwd } => (
            format!("Remove project {name}?"),
            Line::from(vec![
                Span::styled(
                    "Hide removes from view; Delete removes from disk: ",
                    Style::default().fg(theme.foreground),
                ),
                Span::styled(
                    display_dir_path(cwd),
                    Style::default().fg(theme.status_threads),
                ),
            ]),
        ),
        DeleteTarget::Chat { thread } => (
            format!("Delete chat {}?", thread.title()),
            Line::styled(
                "Will delete this chat's on-disk history file(s).",
                Style::default().fg(theme.foreground),
            ),
        ),
    };
    let no_style = delete_choice_style(prompt.choice, DeleteChoice::No, theme);
    let hide_style = delete_choice_style(prompt.choice, DeleteChoice::Hide, theme);
    let delete_style = delete_choice_style(prompt.choice, DeleteChoice::Delete, theme);
    let text = Text::from(vec![
        Line::styled(title, Style::default().fg(theme.error)),
        warning,
        Line::from(""),
        delete_choice_line(prompt, no_style, hide_style, delete_style),
    ]);
    frame.render_widget(
        Paragraph::new(text)
            .block(panel(" Confirm delete ", true, theme))
            .style(Style::default().fg(theme.foreground)),
        popup,
    );
}

fn delete_choice_style(
    selected: DeleteChoice,
    choice: DeleteChoice,
    theme: ResolvedTheme,
) -> Style {
    if selected == choice {
        let bg = if choice == DeleteChoice::Delete {
            theme.error
        } else {
            theme.selected
        };
        Style::default()
            .fg(theme.foreground)
            .bg(bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.muted)
    }
}

fn delete_choice_line(
    prompt: &DeletePrompt,
    no_style: Style,
    hide_style: Style,
    delete_style: Style,
) -> Line<'static> {
    match prompt.target {
        DeleteTarget::Project { .. } => Line::from(vec![
            Span::raw("  "),
            Span::styled(" No ", no_style),
            Span::raw("  "),
            Span::styled(" Hide ", hide_style),
            Span::raw("  "),
            Span::styled(" Delete ", delete_style),
        ]),
        DeleteTarget::Chat { .. } => Line::from(vec![
            Span::raw("  "),
            Span::styled(" No ", no_style),
            Span::raw("  "),
            Span::styled(" Delete ", delete_style),
        ]),
    }
}

fn draw_new_project_prompt(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &App,
    theme: ResolvedTheme,
) {
    let popup = centered(area, 76, 5);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(format!(" {}█", app.new_project_path))
            .block(panel(" New project path ", true, theme))
            .style(Style::default().fg(theme.foreground)),
        popup,
    );
}

fn draw_new_chat_prompt(frame: &mut ratatui::Frame, area: Rect, app: &App, theme: ResolvedTheme) {
    let popup = if app.new_chat_picking_harness {
        centered(area, 96, app.harnesses.len() as u16 + 4)
    } else {
        centered(area, 84, 5)
    };
    frame.render_widget(Clear, popup);
    if app.new_chat_picking_harness {
        let lines = app
            .harnesses
            .iter()
            .enumerate()
            .map(|(index, harness)| {
                let harness_color = new_chat_harness_color(&harness.id, theme);
                let selected = index == app.new_chat_harness_index;
                let unfocused_style = Style::default().fg(theme.new_chat_unfocused);
                let selected_harness_style = Style::default()
                    .fg(harness_color)
                    .bg(theme.selected)
                    .add_modifier(Modifier::BOLD);
                let icon_style = if selected {
                    selected_harness_style
                } else {
                    Style::default().fg(harness_color)
                };
                let label_style = if selected {
                    selected_harness_style
                } else {
                    unfocused_style
                };
                let command_path = harness
                    .command_path
                    .as_deref()
                    .map(display_path)
                    .unwrap_or_else(|| "-".into());
                let (path_prefix, executable) = split_executable_path(&command_path);
                let path_style = if selected {
                    Style::default().fg(theme.new_chat_path).bg(theme.selected)
                } else {
                    unfocused_style
                };
                let executable_style = if selected {
                    Style::default()
                        .fg(theme.new_chat_executable)
                        .bg(theme.selected)
                } else {
                    unfocused_style
                };
                Line::from(vec![
                    Span::styled(format!(" {:<2} ", harness.marker), icon_style),
                    Span::styled(format!("{:<14}", harness.label), label_style),
                    Span::styled(format!(" {path_prefix}"), path_style),
                    Span::styled(format!("{executable} "), executable_style),
                ])
            })
            .collect::<Vec<_>>();
        let block = if let Some(error) = &app.new_chat_harness_error {
            Block::default()
                .title(format!(" {error} "))
                .title_style(
                    Style::default()
                        .fg(theme.error)
                        .add_modifier(Modifier::BOLD),
                )
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.error))
                .style(Style::default().fg(theme.foreground))
        } else {
            panel(" Select Harness ", true, theme)
        };
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(block)
                .style(Style::default().fg(theme.foreground)),
            popup,
        );
        return;
    }
    frame.render_widget(
        Paragraph::new(format!(" {}█", app.new_chat_name))
            .block(panel(" New chat name ", true, theme))
            .style(Style::default().fg(theme.foreground)),
        popup,
    );
}

#[derive(Clone, Copy)]
struct ResolvedTheme {
    foreground: Color,
    muted: Color,
    accent: Color,
    error: Color,
    title_focused: Color,
    title_unfocused: Color,
    border_focused: Color,
    border_unfocused: Color,
    status_projects: Color,
    status_threads: Color,
    status_open: Color,
    status_new: Color,
    status_new_chat: Color,
    status_search: Color,
    status_archive: Color,
    status_archive_action: Color,
    status_unarchive: Color,
    status_delete: Color,
    status_help: Color,
    archive_icon: Color,
    preview_user: Color,
    preview_text: Color,
    preview_title: Color,
    new_chat_unfocused: Color,
    new_chat_pi: Color,
    new_chat_claude: Color,
    new_chat_codex: Color,
    new_chat_cursor: Color,
    new_chat_opencode: Color,
    new_chat_path: Color,
    new_chat_executable: Color,
    selected: Color,
    success: Color,
    warning: Color,
}

impl From<&ThemeConfig> for ResolvedTheme {
    fn from(value: &ThemeConfig) -> Self {
        Self {
            foreground: color(&value.foreground),
            muted: color(&value.muted),
            accent: color(&value.accent),
            error: color(&value.error),
            title_focused: color(&value.title_focused),
            title_unfocused: color(&value.title_unfocused),
            border_focused: color(&value.border_focused),
            border_unfocused: color(&value.border_unfocused),
            status_projects: color(&value.status_projects),
            status_threads: color(&value.status_threads),
            status_open: color(&value.status_open),
            status_new: color(&value.status_new),
            status_new_chat: color(&value.status_new_chat),
            status_search: color(&value.status_search),
            status_archive: color(&value.status_archive),
            status_archive_action: color(&value.status_archive_action),
            status_unarchive: color(&value.status_unarchive),
            status_delete: color(&value.status_delete),
            status_help: color(&value.status_help),
            archive_icon: color(&value.archive_icon),
            preview_user: color(&value.preview_user),
            preview_text: color(&value.preview_text),
            preview_title: color(&value.preview_title),
            new_chat_unfocused: color(&value.new_chat_unfocused),
            new_chat_pi: color(&value.new_chat_pi),
            new_chat_claude: color(&value.new_chat_claude),
            new_chat_codex: color(&value.new_chat_codex),
            new_chat_cursor: color(&value.new_chat_cursor),
            new_chat_opencode: color(&value.new_chat_opencode),
            new_chat_path: color(&value.new_chat_path),
            new_chat_executable: color(&value.new_chat_executable),
            selected: color(&value.selected),
            success: color(&value.success),
            warning: color(&value.warning),
        }
    }
}

fn color(value: &str) -> Color {
    let value = value.trim_start_matches('#');
    if value.len() == 6 {
        if let Ok(number) = u32::from_str_radix(value, 16) {
            return Color::Rgb((number >> 16) as u8, (number >> 8) as u8, number as u8);
        }
    }
    Color::Reset
}

fn panel<'a>(title: &'a str, focused: bool, theme: ResolvedTheme) -> Block<'a> {
    Block::default()
        .title(title)
        .title_style(Style::default().fg(if focused {
            theme.title_focused
        } else {
            theme.title_unfocused
        }))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused {
            theme.border_focused
        } else {
            theme.border_unfocused
        }))
        .style(Style::default().fg(theme.foreground))
}

fn selected(theme: ResolvedTheme) -> Style {
    Style::default()
        .bg(theme.selected)
        .add_modifier(Modifier::BOLD)
}

fn fuzzy_matches(value: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let mut chars = value.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .all(|query_char| chars.any(|value_char| value_char == query_char))
}

fn contains_ignore_case(value: &str, query: &str) -> bool {
    value.to_lowercase().contains(query)
}

fn project_name(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(cwd)
        .to_owned()
}

fn display_dir_path(path: &str) -> String {
    let mut display = display_path(path);
    if !display.ends_with('/') {
        display.push('/');
    }
    display
}

fn display_path(path: &str) -> String {
    let Some(home) = env::var_os("HOME").map(PathBuf::from) else {
        return path.to_owned();
    };
    let path = Path::new(path);
    path.strip_prefix(&home)
        .ok()
        .map(|relative| {
            if relative.as_os_str().is_empty() {
                "~".to_owned()
            } else {
                format!("~/{}", relative.display())
            }
        })
        .unwrap_or_else(|| path.display().to_string())
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn expand_user_path(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(path)
}

fn new_project_path(path: &str) -> PathBuf {
    let expanded = expand_user_path(path);
    if is_bare_project_name(&expanded) {
        state_dir().join(&expanded)
    } else {
        expanded
    }
}

fn is_bare_project_name(path: &Path) -> bool {
    path.is_relative() && path.components().count() == 1
}

fn remove_path_from_disk(path: &Path) -> Result<()> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))
        }
        Ok(_) => {
            fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

fn window_matches_archived_thread(
    window: &Window,
    archived_thread_ids: &HashSet<(String, String)>,
    archived_thread_names: &HashSet<(String, String, String)>,
) -> bool {
    if let (Some(harness_id), Some(thread_id)) =
        (window.harness_id.as_ref(), window.thread_id.as_ref())
    {
        return archived_thread_ids.contains(&(harness_id.clone(), thread_id.clone()));
    }
    if let (Some(harness_id), Some(chat_title)) =
        (window.harness_id.as_ref(), window.chat_title.as_ref())
    {
        let cwd = crate::tmux::normalized_path(&window.cwd)
            .to_string_lossy()
            .into_owned();
        return archived_thread_names.contains(&(harness_id.clone(), chat_title.clone(), cwd));
    }
    false
}

fn row_harness_id(row: &Row) -> Option<&str> {
    match row {
        Row::Agent(window) => window.harness_id.as_deref(),
        Row::Thread { thread, .. } => Some(thread.harness_id.as_str()),
    }
}

fn next_focus(focus: Focus) -> Focus {
    match focus {
        Focus::Projects => Focus::Threads,
        Focus::Threads => Focus::Preview,
        Focus::Preview => Focus::Projects,
    }
}
fn previous_focus(focus: Focus) -> Focus {
    match focus {
        Focus::Projects => Focus::Preview,
        Focus::Threads => Focus::Projects,
        Focus::Preview => Focus::Threads,
    }
}
fn move_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        0
    } else {
        (current as isize + delta).rem_euclid(len as isize) as usize
    }
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x
        && x < area.x.saturating_add(area.width)
        && y >= area.y
        && y < area.y.saturating_add(area.height)
}

fn list_index_at(y: u16, area: Rect) -> Option<usize> {
    let inner_top = area.y.saturating_add(1);
    let inner_bottom = area.y.saturating_add(area.height).saturating_sub(1);
    (y >= inner_top && y < inner_bottom).then_some((y - inner_top) as usize)
}

fn status_action_at(app: &App, x: u16, width: u16) -> Option<StatusAction> {
    let project_count = app.projects.len();
    let thread_count = app
        .projects
        .iter()
        .map(|project| project.threads.len())
        .sum::<usize>();
    let count_status = format!("{project_count} projects · {thread_count} threads");
    let mut offset = 0usize;
    offset += 1;
    offset += format!(" {project_count}").chars().count();
    offset += STATUS_GAP.chars().count();
    offset += format!("󰻞 {thread_count}").chars().count();
    if app.status != count_status {
        offset += STATUS_GAP.chars().count();
        offset += app.status.chars().count();
    }

    let mut cursor = offset;
    for (label, action) in status_actions_left(app) {
        cursor += STATUS_GAP.chars().count();
        let end = cursor + label.chars().count();
        if (cursor..end).contains(&(x as usize)) {
            return Some(action);
        }
        cursor = end;
    }

    let right_actions = status_actions_right(app);
    let right_width: usize = right_actions
        .iter()
        .enumerate()
        .map(|(index, (label, _))| {
            label.chars().count()
                + if index == 0 {
                    0
                } else {
                    STATUS_GAP.chars().count()
                }
        })
        .sum();
    let mut cursor = (width as usize).saturating_sub(right_width);
    for (index, (label, action)) in right_actions.into_iter().enumerate() {
        if index > 0 {
            cursor += STATUS_GAP.chars().count();
        }
        let end = cursor + label.chars().count();
        if (cursor..end).contains(&(x as usize)) {
            return Some(action);
        }
        cursor = end;
    }
    None
}

fn status_actions_left(app: &App) -> Vec<(String, StatusAction)> {
    vec![
        ("󰧮 󰋖".into(), StatusAction::Help),
        (status_search_label(app), StatusAction::Search),
    ]
}

fn status_search_label(app: &App) -> String {
    if app.search_mode {
        format!(" {}█", app.query)
    } else if !app.query.is_empty() {
        format!(" {}", app.query)
    } else {
        " /".into()
    }
}

fn status_actions_right(app: &App) -> Vec<(&'static str, StatusAction)> {
    let archive_action = if app.show_archived {
        (" (U)narchive", StatusAction::SetUnarchived)
    } else {
        (" (A)rchive", StatusAction::SetArchived)
    };
    vec![
        ("󰷏 (enter)", StatusAction::Open),
        ("󰁌 (a)ll", StatusAction::ToggleArchived),
        (" (N)ew Project", StatusAction::NewProject),
        (" (n)ew Chat", StatusAction::New),
        archive_action,
        (" (D)elete", StatusAction::Delete),
    ]
}

fn status_color(action: StatusAction, theme: ResolvedTheme) -> Color {
    match action {
        StatusAction::Open => theme.status_open,
        StatusAction::NewProject => theme.status_new,
        StatusAction::New => theme.status_new_chat,
        StatusAction::Search => theme.status_search,
        StatusAction::ToggleArchived => theme.status_archive,
        StatusAction::SetArchived => theme.status_archive_action,
        StatusAction::SetUnarchived => theme.status_unarchive,
        StatusAction::Delete => theme.status_delete,
        StatusAction::Help => theme.status_help,
    }
}

fn split_executable_path(path: &str) -> (&str, &str) {
    path.rsplit_once('/')
        .map(|(prefix, executable)| (&path[..prefix.len() + 1], executable))
        .unwrap_or(("", path))
}

fn new_chat_harness_color(harness_id: &str, theme: ResolvedTheme) -> Color {
    match harness_id {
        PI_HARNESS_ID => theme.new_chat_pi,
        CLAUDE_HARNESS_ID => theme.new_chat_claude,
        CODEX_HARNESS_ID => theme.new_chat_codex,
        CURSOR_HARNESS_ID => theme.new_chat_cursor,
        OPENCODE_HARNESS_ID => theme.new_chat_opencode,
        _ => theme.muted,
    }
}

fn harness_index_at(x: u16, y: u16, popup: Rect, harnesses: &[Harness]) -> Option<usize> {
    if x <= popup.x || x >= popup.x.saturating_add(popup.width).saturating_sub(1) {
        return None;
    }
    let first_row = popup.y.saturating_add(1);
    let index = y.checked_sub(first_row)? as usize;
    (index < harnesses.len()).then_some(index)
}

fn deleted_chat_agent_pane_matches(
    window: &Window,
    thread: &Thread,
    agent_window_names: &[String],
) -> bool {
    if !agent_window_names
        .iter()
        .any(|name| name == &window.window_name)
        || crate::tmux::normalized_path(&window.cwd) != crate::tmux::normalized_path(&thread.cwd)
        || window.harness_id.as_deref() != Some(thread.harness_id.as_str())
    {
        return false;
    }

    if window.thread_id.as_deref() == Some(thread.id.as_str()) {
        return true;
    }

    let Some(chat_title) = window.chat_title.as_deref() else {
        return false;
    };
    chat_title == thread.title() || thread.name.as_deref() == Some(chat_title)
}

fn delete_choices(target: &DeleteTarget) -> &'static [DeleteChoice] {
    match target {
        DeleteTarget::Project { .. } => {
            &[DeleteChoice::No, DeleteChoice::Hide, DeleteChoice::Delete]
        }
        DeleteTarget::Chat { .. } => &[DeleteChoice::No, DeleteChoice::Delete],
    }
}

fn default_delete_choice(target: &DeleteTarget) -> DeleteChoice {
    match target {
        DeleteTarget::Project { .. } => DeleteChoice::Hide,
        DeleteTarget::Chat { .. } => DeleteChoice::Delete,
    }
}

fn previous_delete_choice(choice: DeleteChoice, target: &DeleteTarget) -> DeleteChoice {
    let choices = delete_choices(target);
    let index = choices
        .iter()
        .position(|value| *value == choice)
        .unwrap_or(0);
    choices[move_index(index, choices.len(), -1)]
}

fn next_delete_choice(choice: DeleteChoice, target: &DeleteTarget) -> DeleteChoice {
    let choices = delete_choices(target);
    let index = choices
        .iter()
        .position(|value| *value == choice)
        .unwrap_or(0);
    choices[move_index(index, choices.len(), 1)]
}

fn delete_choice_at(x: u16, y: u16, popup: Rect, target: &DeleteTarget) -> Option<DeleteChoice> {
    if y != popup.y.saturating_add(4) {
        return None;
    }
    let no_start = popup.x.saturating_add(3);
    let no_end = no_start.saturating_add(4);
    if x >= no_start && x < no_end {
        return Some(DeleteChoice::No);
    }
    let second_start = popup.x.saturating_add(9);
    if matches!(target, DeleteTarget::Chat { .. }) {
        let second_end = second_start.saturating_add(8);
        if x >= second_start && x < second_end {
            return Some(DeleteChoice::Delete);
        }
        return None;
    }
    let hide_end = second_start.saturating_add(6);
    if x >= second_start && x < hide_end {
        return Some(DeleteChoice::Hide);
    }
    let delete_start = popup.x.saturating_add(17);
    let delete_end = delete_start.saturating_add(8);
    if x >= delete_start && x < delete_end {
        return Some(DeleteChoice::Delete);
    }
    None
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

fn format_timestamp(timestamp: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(timestamp)
        .map(|date| {
            format!(
                "{:04}-{:02}-{:02}",
                date.year(),
                u8::from(date.month()),
                date.day()
            )
        })
        .unwrap_or_else(|_| timestamp.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn index_wraps() {
        assert_eq!(move_index(0, 3, -1), 2);
        assert_eq!(move_index(2, 3, 1), 0);
    }
    #[test]
    fn parses_rgb_theme_colors() {
        assert_eq!(color("#112233"), Color::Rgb(0x11, 0x22, 0x33));
    }
    #[test]
    fn formats_thread_dates() {
        assert_eq!(format_timestamp(0), "1970-01-01");
    }
    #[test]
    fn finds_delete_prompt_mouse_choices() {
        let popup = Rect::new(10, 5, 76, 8);
        let project = DeleteTarget::Project {
            name: "repo".into(),
            cwd: "/tmp/repo".into(),
        };
        let chat = DeleteTarget::Chat {
            thread: Thread {
                harness_id: "codex".into(),
                id: "thread".into(),
                name: Some("chat".into()),
                preview: String::new(),
                cwd: "/tmp/repo".into(),
                created_at: 0,
                updated_at: 0,
                recency_at: None,
                source: serde_json::Value::Null,
                git_info: None,
                archived: false,
                path: None,
            },
        };
        assert_eq!(
            delete_choice_at(13, 9, popup, &project),
            Some(DeleteChoice::No)
        );
        assert_eq!(
            delete_choice_at(19, 9, popup, &project),
            Some(DeleteChoice::Hide)
        );
        assert_eq!(
            delete_choice_at(28, 9, popup, &project),
            Some(DeleteChoice::Delete)
        );
        assert_eq!(
            delete_choice_at(19, 9, popup, &chat),
            Some(DeleteChoice::Delete)
        );
        assert_eq!(delete_choice_at(18, 9, popup, &project), None);
        assert_eq!(delete_choice_at(13, 8, popup, &project), None);
    }

    #[test]
    fn matches_deleted_chat_agent_panes_by_thread_or_name() {
        let thread = Thread {
            harness_id: "codex".into(),
            id: "thread-1".into(),
            name: Some("Fix bug".into()),
            preview: String::new(),
            cwd: "/tmp/repo".into(),
            created_at: 0,
            updated_at: 0,
            recency_at: None,
            source: serde_json::Value::Null,
            git_info: None,
            archived: false,
            path: None,
        };
        let agent_window_names = vec!["agents".to_owned()];
        let mut window = Window {
            session: "repo".into(),
            session_last_attached: 0,
            window_id: "@1".into(),
            window_name: "agents".into(),
            pane_id: "%1".into(),
            pane_pid: 1,
            command: "codex".into(),
            cwd: "/tmp/repo".into(),
            harness_id: Some("codex".into()),
            thread_id: Some("thread-1".into()),
            chat_title: None,
        };

        assert!(deleted_chat_agent_pane_matches(
            &window,
            &thread,
            &agent_window_names
        ));

        window.thread_id = None;
        window.chat_title = Some("Fix bug".into());
        assert!(deleted_chat_agent_pane_matches(
            &window,
            &thread,
            &agent_window_names
        ));

        window.window_name = "other".into();
        assert!(!deleted_chat_agent_pane_matches(
            &window,
            &thread,
            &agent_window_names
        ));
    }
}
