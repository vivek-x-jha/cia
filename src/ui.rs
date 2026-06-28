use std::{
    collections::HashSet,
    fs, io,
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
    layout::{Constraint, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Wrap,
    },
    Terminal,
};

use crate::{
    agent::{Harness, Message, Thread, PI_HARNESS_ID},
    config::{state_path, Config, ThemeConfig},
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

#[derive(Clone, Debug)]
struct DeletePrompt {
    target: DeleteTarget,
    yes_selected: bool,
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
    new_chat_mode: bool,
    new_chat_picking_harness: bool,
    new_chat_harness_index: usize,
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
            new_chat_mode: false,
            new_chat_picking_harness: false,
            new_chat_harness_index: 0,
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
        let archived_thread_ids: HashSet<(String, String)> = archived_threads
            .iter()
            .map(|thread| (thread.harness_id.clone(), thread.id.clone()))
            .collect();
        let archived_thread_names: HashSet<(String, String, String)> = archived_threads
            .iter()
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
        let mut threads = active_threads;
        if self.show_archived {
            threads.append(&mut archived_threads);
        }
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
            .map(|harness| (harness.id.clone(), harness.command.clone()))
            .context("thread belongs to an unavailable harness")?;
        let window = self.tmux.open_agent(AgentLaunch {
            inventory: &self.all_windows,
            cwd: &thread.cwd,
            title: thread.title(),
            harness_id: &harness_id,
            thread_id: Some(&thread.id),
            cia_command: &self.cia_command,
            agent_command: &harness_command,
        })?;
        self.state.record(&harness_id, &thread.id, &window);
        self.state.last_project = Some(thread.cwd.clone());
        self.state.save(&self.state_path)?;
        self.tmux.switch_to(&window)
    }

    fn begin_new_thread(&mut self) {
        self.new_chat_name.clear();
        self.new_chat_harness_index = self
            .harnesses
            .iter()
            .position(|harness| harness.id == PI_HARNESS_ID)
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

    fn set_selected_archived(&mut self, archived: bool) {
        let Some(Row::Thread { thread, .. }) = self.current_rows().get(self.row_index).cloned()
        else {
            self.status = "Select a saved chat first".into();
            return;
        };
        if thread.archived == archived {
            self.refresh_view();
            return;
        }
        let result = self
            .harness_mut(&thread.harness_id)
            .context("thread belongs to an unavailable harness")
            .and_then(|harness| harness.set_archived(&thread.id, archived));
        match result {
            Ok(()) => self.refresh_view(),
            Err(error) => self.status = error.to_string(),
        }
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
            yes_selected: false,
        });
    }

    fn confirm_delete(&mut self) {
        let Some(prompt) = self.delete_prompt.take() else {
            return;
        };
        let result = match prompt.target {
            DeleteTarget::Project { cwd, .. } => remove_path_from_disk(Path::new(&cwd)),
            DeleteTarget::Chat { thread } => self.delete_chat_and_folder(&thread),
        };
        match result {
            Ok(()) => {
                self.refresh_view();
                self.status = "Deleted".into();
            }
            Err(error) => self.status = error.to_string(),
        }
    }

    fn delete_chat_and_folder(&mut self, thread: &Thread) -> Result<()> {
        self.harness_mut(&thread.harness_id)
            .context("thread belongs to an unavailable harness")?
            .delete_thread(&thread.id)?;
        remove_path_from_disk(Path::new(&thread.cwd))
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
            StatusAction::New => self.begin_new_thread(),
            StatusAction::Search => self.search_mode = true,
            StatusAction::ToggleArchived => self.toggle_archived(),
            StatusAction::SetArchived => self.set_selected_archived(true),
            StatusAction::SetUnarchived => self.set_selected_archived(false),
            StatusAction::Delete => self.begin_delete(),
            StatusAction::Help => self.show_help = true,
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
        let Some((harness_id, harness_command)) = self
            .harnesses
            .get(self.new_chat_harness_index)
            .map(|harness| (harness.id.clone(), harness.command.clone()))
        else {
            self.status = "Selected harness is unavailable".into();
            return;
        };
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
                KeyCode::Enter => {
                    if self
                        .delete_prompt
                        .as_ref()
                        .is_some_and(|prompt| prompt.yes_selected)
                    {
                        self.confirm_delete();
                    } else {
                        self.delete_prompt = None;
                    }
                }
                KeyCode::Left | KeyCode::Char('h') | KeyCode::Right | KeyCode::Char('l') => {
                    if let Some(prompt) = &mut self.delete_prompt {
                        prompt.yes_selected = !prompt.yes_selected;
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') => self.delete_prompt = None,
                KeyCode::Char('y') | KeyCode::Char('Y') => self.confirm_delete(),
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
                        self.new_chat_name.clear();
                    }
                    KeyCode::Enter => self.new_chat_picking_harness = false,
                    KeyCode::Left | KeyCode::Char('h') | KeyCode::Up | KeyCode::Char('k') => {
                        self.new_chat_harness_index =
                            move_index(self.new_chat_harness_index, self.harnesses.len(), -1);
                    }
                    KeyCode::Right | KeyCode::Char('l') | KeyCode::Down | KeyCode::Char('j') => {
                        self.new_chat_harness_index =
                            move_index(self.new_chat_harness_index, self.harnesses.len(), 1);
                    }
                    _ => {}
                }
                return;
            }
            match key.code {
                KeyCode::Esc => {
                    self.new_chat_mode = false;
                    self.new_chat_picking_harness = false;
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
            KeyCode::Char('d') if key.modifiers == KeyModifiers::NONE => self.begin_delete(),
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
            KeyCode::Char('n') => self.begin_new_thread(),
            KeyCode::Char('G') => self.select_boundary(true),
            KeyCode::Enter => self.activate(),
            KeyCode::Down | KeyCode::Char('j') => self.select_next(1),
            KeyCode::Up | KeyCode::Char('k') => self.select_next(-1),
            KeyCode::Tab | KeyCode::Char('l') => self.focus = next_focus(self.focus),
            KeyCode::BackTab | KeyCode::Char('h') => self.focus = previous_focus(self.focus),
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
            if let Some(action) = status_action_at(self, x.saturating_sub(areas.status.x)) {
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

    fn handle_new_chat_click(&mut self, x: u16, y: u16) {
        let Ok((width, height)) = terminal_size() else {
            return;
        };
        let popup = centered(Rect::new(0, 0, width, height), 60, 5);
        if !contains(popup, x, y) {
            return;
        }
        if self.new_chat_picking_harness {
            if let Some(index) = harness_index_at(x, y, popup, &self.harnesses) {
                self.new_chat_harness_index = index;
                self.new_chat_picking_harness = false;
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
    let search = if app.search_mode {
        format!(" /{}█", app.query)
    } else if !app.query.is_empty() {
        format!(" /{}", app.query)
    } else {
        String::new()
    };
    let project_count = app.projects.len();
    let thread_count = app
        .projects
        .iter()
        .map(|project| project.threads.len())
        .sum::<usize>();
    let count_status = format!("{project_count} projects · {thread_count} threads");
    let mut spans = vec![
        Span::styled(" ", Style::default().fg(theme.muted)),
        Span::styled(
            project_count.to_string(),
            Style::default().fg(theme.status_projects),
        ),
        Span::styled(" Projects", Style::default().fg(theme.status_projects)),
        Span::styled(" · ", Style::default().fg(theme.muted)),
        Span::styled(
            thread_count.to_string(),
            Style::default().fg(theme.status_threads),
        ),
        Span::styled(" threads", Style::default().fg(theme.status_threads)),
    ];
    if app.status != count_status {
        spans.push(Span::styled(" · ", Style::default().fg(theme.muted)));
        spans.push(Span::styled(
            app.status.clone(),
            Style::default().fg(theme.error),
        ));
    }
    spans.push(Span::styled(search, Style::default().fg(theme.warning)));
    for (label, action) in status_actions(app) {
        spans.push(Span::styled(" · ", Style::default().fg(theme.muted)));
        spans.push(Span::styled(
            label,
            Style::default().fg(status_color(action, theme)),
        ));
    }
    spans.push(Span::styled(" ", Style::default().fg(theme.muted)));
    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
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
        .enumerate()
        .map(|(index, row)| {
            let marker = if row.is_live() {
                Span::styled("● ", Style::default().fg(theme.success))
            } else {
                Span::styled("  ", Style::default().fg(theme.muted))
            };
            let harness = row_harness_id(row)
                .and_then(|harness_id| app.harness(harness_id))
                .map(|harness| harness.label.as_str())
                .unwrap_or("?");
            let harness_color = if index == app.row_index {
                Color::Cyan
            } else {
                theme.muted
            };
            ListItem::new(Line::from(vec![
                marker,
                Span::styled(format!("{harness} "), Style::default().fg(harness_color)),
                Span::raw(row.title()),
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
                        .fg(theme.accent)
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
                        } else if thread.harness_id == PI_HARNESS_ID {
                            harness
                                .map(|harness| harness.marker.as_str())
                                .unwrap_or("π")
                        } else {
                            &message.role
                        };
                        text.push_line(Line::styled(
                            role.to_string(),
                            Style::default()
                                .fg(if is_user {
                                    theme.preview_user
                                } else if thread.harness_id == PI_HARNESS_ID {
                                    theme.preview_pi
                                } else {
                                    theme.preview_codex
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
    let popup = centered(area, 64, 18);
    frame.render_widget(Clear, popup);
    let help = "Navigation\n  Tab / h / l       change pane\n  j / Ctrl+n / down move selection down\n  k / Ctrl+p / up   move selection up\n  Ctrl+d / Ctrl+u   scroll preview\n  gg / G             first / last selection\n  Enter              switch or resume\n\nActions\n  n new chat    / search    a active/all\n  A archive     U unarchive  d delete\n  r refresh     q/Esc close  ? help";
    frame.render_widget(
        Paragraph::new(help)
            .block(panel(" CIA Help ", true, theme))
            .style(Style::default().fg(theme.foreground)),
        popup,
    );
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
            format!("Delete project {name}?"),
            format!("Will remove project folder {cwd} from disk."),
        ),
        DeleteTarget::Chat { thread } => (
            format!("Delete chat {}?", thread.title()),
            format!(
                "Will delete saved chat and remove {} from disk.",
                thread.cwd
            ),
        ),
    };
    let no_style = if !prompt.yes_selected {
        Style::default()
            .fg(theme.foreground)
            .bg(theme.selected)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.muted)
    };
    let yes_style = if prompt.yes_selected {
        Style::default()
            .fg(theme.foreground)
            .bg(theme.error)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.muted)
    };
    let text = Text::from(vec![
        Line::from(title),
        Line::styled(warning, Style::default().fg(theme.warning)),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(" No ", no_style),
            Span::raw("  "),
            Span::styled(" Yes ", yes_style),
        ]),
    ]);
    frame.render_widget(
        Paragraph::new(text)
            .block(panel(" Confirm delete ", true, theme))
            .style(Style::default().fg(theme.foreground)),
        popup,
    );
}

fn draw_new_chat_prompt(frame: &mut ratatui::Frame, area: Rect, app: &App, theme: ResolvedTheme) {
    let popup = centered(area, 60, 5);
    frame.render_widget(Clear, popup);
    if app.new_chat_picking_harness {
        let spans = app
            .harnesses
            .iter()
            .enumerate()
            .flat_map(|(index, harness)| {
                let style = if index == app.new_chat_harness_index {
                    Style::default()
                        .fg(theme.foreground)
                        .bg(theme.selected)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.muted)
                };
                [
                    Span::raw(" "),
                    Span::styled(format!(" {} ", harness.label), style),
                ]
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(Line::from(spans))
                .block(panel(" New chat harness ", true, theme))
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
    status_projects: Color,
    status_threads: Color,
    status_open: Color,
    status_new: Color,
    status_search: Color,
    status_archive: Color,
    status_archive_action: Color,
    status_unarchive: Color,
    status_delete: Color,
    status_help: Color,
    preview_user: Color,
    preview_codex: Color,
    preview_pi: Color,
    preview_text: Color,
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
            status_projects: color(&value.status_projects),
            status_threads: color(&value.status_threads),
            status_open: color(&value.status_open),
            status_new: color(&value.status_new),
            status_search: color(&value.status_search),
            status_archive: color(&value.status_archive),
            status_archive_action: color(&value.status_archive_action),
            status_unarchive: color(&value.status_unarchive),
            status_delete: color(&value.status_delete),
            status_help: color(&value.status_help),
            preview_user: color(&value.preview_user),
            preview_codex: color(&value.preview_codex),
            preview_pi: color(&value.preview_pi),
            preview_text: color(&value.preview_text),
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
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused { theme.accent } else { theme.muted }))
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

fn status_action_at(app: &App, x: u16) -> Option<StatusAction> {
    let search = if app.search_mode {
        format!(" /{}█", app.query)
    } else if !app.query.is_empty() {
        format!(" /{}", app.query)
    } else {
        String::new()
    };
    let project_count = app.projects.len();
    let thread_count = app
        .projects
        .iter()
        .map(|project| project.threads.len())
        .sum::<usize>();
    let count_status = format!("{project_count} projects · {thread_count} threads");
    let mut offset = 0usize;
    offset += 1;
    offset += project_count.to_string().chars().count();
    offset += " Projects".chars().count();
    offset += " · ".chars().count();
    offset += thread_count.to_string().chars().count();
    offset += " threads".chars().count();
    if app.status != count_status {
        offset += " · ".chars().count();
        offset += app.status.chars().count();
    }
    offset += search.chars().count();

    let mut cursor = offset;
    for (label, action) in status_actions(app) {
        cursor += " · ".chars().count();
        let end = cursor + label.chars().count();
        if (cursor..end).contains(&(x as usize)) {
            return Some(action);
        }
        cursor = end;
    }
    None
}

fn status_actions(app: &App) -> Vec<(&'static str, StatusAction)> {
    let archive_action = if app.show_archived {
        ("Unarchive (U)", StatusAction::SetUnarchived)
    } else {
        ("Archive (A)", StatusAction::SetArchived)
    };
    vec![
        ("Search (/)", StatusAction::Search),
        ("Open (Enter)", StatusAction::Open),
        ("New (n)", StatusAction::New),
        ("All (a)", StatusAction::ToggleArchived),
        archive_action,
        ("Delete (d)", StatusAction::Delete),
        ("Help (?)", StatusAction::Help),
    ]
}

fn status_color(action: StatusAction, theme: ResolvedTheme) -> Color {
    match action {
        StatusAction::Open => theme.status_open,
        StatusAction::New => theme.status_new,
        StatusAction::Search => theme.status_search,
        StatusAction::ToggleArchived => theme.status_archive,
        StatusAction::SetArchived => theme.status_archive_action,
        StatusAction::SetUnarchived => theme.status_unarchive,
        StatusAction::Delete => theme.status_delete,
        StatusAction::Help => theme.status_help,
    }
}

fn harness_index_at(x: u16, y: u16, popup: Rect, harnesses: &[Harness]) -> Option<usize> {
    if y != popup.y.saturating_add(1) {
        return None;
    }
    let mut cursor = popup.x.saturating_add(1);
    for (index, harness) in harnesses.iter().enumerate() {
        cursor = cursor.saturating_add(1);
        let width = (harness.label.chars().count() + 2) as u16;
        let end = cursor.saturating_add(width);
        if x >= cursor && x < end {
            return Some(index);
        }
        cursor = end;
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
}
