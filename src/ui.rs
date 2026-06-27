use std::{io, path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
        MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
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
    codex::{Client as CodexClient, Message, Thread},
    config::{state_path, Config, ThemeConfig},
    model::{build_projects, rows, Project, Row},
    state::State,
    tmux::{Client as TmuxClient, Window},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Projects,
    Threads,
    Preview,
}

pub struct App {
    config: Config,
    cia_command: String,
    username: String,
    codex: CodexClient,
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
    new_chat_name: String,
    preview: Vec<Message>,
    preview_scroll: u16,
    status: String,
    show_help: bool,
    pending_g: bool,
    running: bool,
}

impl App {
    pub fn new(config: Config, preferred_project: Option<PathBuf>) -> Result<Self> {
        let state_path = state_path();
        let state = State::load(&state_path)
            .with_context(|| format!("failed to load {}", state_path.display()))?;
        let codex = CodexClient::start(&config.codex.command)?;
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
            codex,
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
            new_chat_name: String::new(),
            preview: Vec::new(),
            preview_scroll: 0,
            status: String::new(),
            show_help: false,
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
        let threads = self.codex.list_threads(self.show_archived)?;
        let mut windows = self.tmux.inventory()?;
        self.state.reconcile(&mut windows);
        self.all_windows = windows.clone();
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
        Ok(())
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
                    || project.name.to_lowercase().contains(&query)
                    || project.cwd.to_lowercase().contains(&query)
                    || rows(project)
                        .iter()
                        .any(|row| row.title().to_lowercase().contains(&query))
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn current_rows(&self) -> Vec<Row> {
        let query = self.query.to_lowercase();
        self.current_project()
            .map(rows)
            .unwrap_or_default()
            .into_iter()
            .filter(|row| {
                query.is_empty()
                    || self.current_project().is_some_and(|project| {
                        project.name.to_lowercase().contains(&query)
                            || project.cwd.to_lowercase().contains(&query)
                    })
                    || row.title().to_lowercase().contains(&query)
            })
            .collect()
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
            match self
                .codex
                .read_messages(&thread.id, self.config.codex.transcript_turns)
            {
                Ok(messages) => {
                    self.preview = messages;
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
        let window = self.tmux.open_thread(
            &self.all_windows,
            &thread.cwd,
            thread.title(),
            &thread.id,
            &self.cia_command,
            &self.config.codex.command,
        )?;
        self.state.record(&thread.id, &window);
        self.state.last_project = Some(thread.cwd.clone());
        self.state.save(&self.state_path)?;
        self.tmux.switch_to(&window)
    }

    fn begin_new_thread(&mut self) {
        self.new_chat_name.clear();
        self.new_chat_mode = true;
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
        if project
            .threads
            .iter()
            .any(|thread| thread.name.as_deref() == Some(title))
        {
            self.status = format!("A chat named `{title}` already exists in this project");
            return;
        }
        let result = self
            .tmux
            .open_new_thread(
                &self.all_windows,
                &project.cwd,
                title,
                &self.cia_command,
                &self.config.codex.command,
            )
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
        if self.new_chat_mode {
            match key.code {
                KeyCode::Esc => {
                    self.new_chat_mode = false;
                    self.new_chat_name.clear();
                }
                KeyCode::Enter => self.submit_new_thread(),
                KeyCode::Backspace => {
                    self.new_chat_name.pop();
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
            KeyCode::Char('a') => {
                self.show_archived = !self.show_archived;
                if let Err(error) = self.refresh() {
                    self.status = error.to_string();
                }
                self.load_preview();
            }
            KeyCode::Char('r') => {
                if let Err(error) = self.refresh() {
                    self.status = error.to_string();
                }
                self.load_preview();
            }
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
        match mouse.kind {
            MouseEventKind::ScrollDown => self.scroll_preview(3),
            MouseEventKind::ScrollUp => self.scroll_preview(-3),
            _ => {}
        }
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
    let outer = Layout::vertical([Constraint::Length(1), Constraint::Min(5)]).split(area);
    let panes = if area.width >= 100 {
        let rows = Layout::vertical([Constraint::Percentage(36), Constraint::Percentage(64)])
            .split(outer[1]);
        let top = Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)])
            .split(rows[0]);
        vec![top[0], top[1], rows[1]]
    } else {
        Layout::vertical([
            Constraint::Percentage(25),
            Constraint::Percentage(35),
            Constraint::Percentage(40),
        ])
        .split(outer[1])
        .to_vec()
    };
    draw_status_bar(frame, outer[0], app, theme);
    draw_projects(frame, panes[0], app, theme);
    draw_threads(frame, panes[1], app, theme);
    draw_preview(frame, panes[2], app, theme);
    if app.show_help {
        draw_help(frame, area, theme);
    }
    if app.new_chat_mode {
        draw_new_chat_prompt(frame, area, app, theme);
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
    spans.extend([
        Span::styled(search, Style::default().fg(theme.warning)),
        Span::styled(" · ", Style::default().fg(theme.muted)),
        Span::styled("Enter open", Style::default().fg(theme.status_open)),
        Span::styled(" · ", Style::default().fg(theme.muted)),
        Span::styled("n new", Style::default().fg(theme.status_new)),
        Span::styled(" · ", Style::default().fg(theme.muted)),
        Span::styled("/ search", Style::default().fg(theme.status_search)),
        Span::styled(" · ", Style::default().fg(theme.muted)),
        Span::styled("a archive", Style::default().fg(theme.status_archive)),
        Span::styled(" · ", Style::default().fg(theme.muted)),
        Span::styled("? help", Style::default().fg(theme.status_help)),
        Span::styled(" ", Style::default().fg(theme.muted)),
    ]);
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
        .map(|row| {
            let marker = if row.is_live() {
                Span::styled("● ", Style::default().fg(theme.success))
            } else {
                Span::styled("  ", Style::default().fg(theme.muted))
            };
            ListItem::new(Line::from(vec![marker, Span::raw(row.title())]))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.row_index));
    let title = if app.show_archived {
        " Archived "
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
                text.push_line("CIA will switch to this window without guessing which Codex thread it contains.");
            }
            Row::Thread { thread, live } => {
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
                        "{} · {} · created {} · {}",
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
                    text.push_line(if thread.preview.is_empty() {
                        "No transcript preview available.".to_string()
                    } else {
                        thread.preview.clone()
                    });
                } else {
                    for message in &app.preview {
                        let is_user = message.role == "You";
                        let role = if is_user {
                            app.username.as_str()
                        } else {
                            &message.role
                        };
                        text.push_line(Line::styled(
                            role.to_string(),
                            Style::default()
                                .fg(if is_user {
                                    theme.preview_user
                                } else {
                                    theme.preview_codex
                                })
                                .add_modifier(Modifier::BOLD),
                        ));
                        text.push_line(message.text.clone());
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
    let help = "Navigation\n  Tab / h / l       change pane\n  j / Ctrl+n / down move selection down\n  k / Ctrl+p / up   move selection up\n  Ctrl+d / Ctrl+u   scroll preview\n  gg / G             first / last selection\n  Enter              switch or resume\n\nActions\n  n new chat    / search    a archived\n  r refresh     q/Esc close    ? help";
    frame.render_widget(
        Paragraph::new(help)
            .block(panel(" CIA Help ", true, theme))
            .style(Style::default().fg(theme.foreground)),
        popup,
    );
}

fn draw_new_chat_prompt(frame: &mut ratatui::Frame, area: Rect, app: &App, theme: ResolvedTheme) {
    let popup = centered(area, 60, 5);
    frame.render_widget(Clear, popup);
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
    status_help: Color,
    preview_user: Color,
    preview_codex: Color,
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
            status_help: color(&value.status_help),
            preview_user: color(&value.preview_user),
            preview_codex: color(&value.preview_codex),
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
        .fg(theme.foreground)
        .bg(theme.selected)
        .add_modifier(Modifier::BOLD)
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
