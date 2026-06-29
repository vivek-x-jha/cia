use anyhow::Result;
use serde::Deserialize;

use crate::{codex, config::Config, pi};

pub const CODEX_HARNESS_ID: &str = "codex";
pub const PI_HARNESS_ID: &str = "pi";
pub const PI_HARNESS_LABEL: &str = "Pi";
pub const CLAUDE_HARNESS_ID: &str = "claude";
pub const CURSOR_HARNESS_ID: &str = "cursor";
pub const OPENCODE_HARNESS_ID: &str = "opencode";
pub const DEFAULT_HARNESS_ID: &str = CODEX_HARNESS_ID;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    #[serde(default = "default_harness_id")]
    pub harness_id: String,
    pub id: String,
    pub name: Option<String>,
    pub preview: String,
    pub cwd: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub recency_at: Option<i64>,
    #[serde(default)]
    pub context_remaining: Option<ContextRemaining>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub path: Option<String>,
}

fn default_harness_id() -> String {
    DEFAULT_HARNESS_ID.into()
}

#[derive(Clone, Debug, Deserialize)]
pub struct ContextRemaining {
    pub used_tokens: u64,
    pub max_tokens: u64,
}

impl ContextRemaining {
    pub fn percent_left(&self) -> u64 {
        if self.max_tokens == 0 {
            return 0;
        }
        let remaining = self.max_tokens.saturating_sub(self.used_tokens);
        ((remaining as f64 / self.max_tokens as f64) * 100.0).round() as u64
    }
}

impl Thread {
    pub fn title(&self) -> &str {
        self.name
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| {
                self.preview
                    .lines()
                    .next()
                    .filter(|line| !line.is_empty())
                    .unwrap_or("Untitled chat")
            })
    }

    pub fn recency(&self) -> i64 {
        self.recency_at.unwrap_or(self.updated_at)
    }

    pub fn storage_paths(&self) -> impl Iterator<Item = &str> {
        self.path.as_deref().into_iter()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub role: String,
    pub text: String,
}

pub trait Client {
    fn list_threads(&mut self, archived: bool) -> Result<Vec<Thread>>;
    fn read_messages(&mut self, thread_id: &str, turns: usize) -> Result<Vec<Message>>;
}

pub struct Harness {
    pub id: String,
    pub label: String,
    pub marker: String,
    pub command: String,
    pub command_path: Option<String>,
    client: Box<dyn Client>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HarnessKind {
    Codex,
    Pi,
    Basic,
}

impl Harness {
    pub fn start_all(config: &Config) -> Vec<Result<Self>> {
        let mut harnesses = Vec::new();
        if config.claude.enabled.unwrap_or(true) {
            harnesses.push(Ok(Self::basic(
                CLAUDE_HARNESS_ID,
                &config.claude.label,
                &config.claude.icon,
                &config.claude.command,
            )));
        }
        harnesses.push(Self::start_codex(config));
        if config.cursor.enabled.unwrap_or(true) {
            harnesses.push(Ok(Self::basic(
                CURSOR_HARNESS_ID,
                &config.cursor.label,
                &config.cursor.icon,
                &config.cursor.command,
            )));
        }
        if config.opencode.enabled.unwrap_or(true) {
            harnesses.push(Ok(Self::basic(
                OPENCODE_HARNESS_ID,
                &config.opencode.label,
                &config.opencode.icon,
                &config.opencode.command,
            )));
        }
        if config.pi.enabled.unwrap_or(true) {
            harnesses.push(Self::start_pi(config));
        }
        harnesses
    }

    pub fn start_codex(config: &Config) -> Result<Self> {
        let command_path = command_path(&config.codex.command);
        let client: Box<dyn Client> = if command_path.is_some() {
            Box::new(codex::Client::start(&config.codex.command)?)
        } else {
            Box::new(BasicClient)
        };
        Ok(Self {
            id: CODEX_HARNESS_ID.into(),
            label: config.codex.label.clone(),
            marker: config.codex.icon.clone(),
            command: config.codex.command.clone(),
            command_path,
            client,
        })
    }

    pub fn start_pi(config: &Config) -> Result<Self> {
        Ok(Self {
            id: PI_HARNESS_ID.into(),
            label: config.pi.label.clone(),
            marker: config.pi.icon.clone(),
            command: config.pi.command.clone(),
            command_path: command_path(&config.pi.command),
            client: Box::new(pi::Client::new(config.pi.session_dir.clone())),
        })
    }

    fn basic(id: &str, label: &str, marker: &str, command: &str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            marker: marker.into(),
            command: command.into(),
            command_path: command_path(command),
            client: Box::new(BasicClient),
        }
    }

    pub fn list_threads(&mut self, archived: bool) -> Result<Vec<Thread>> {
        let mut threads = self.client.list_threads(archived)?;
        for thread in &mut threads {
            thread.harness_id = self.id.clone();
        }
        Ok(threads)
    }

    pub fn read_messages(&mut self, thread_id: &str, turns: usize) -> Result<Vec<Message>> {
        self.client.read_messages(thread_id, turns)
    }

    pub fn command_available(&self) -> bool {
        self.command_path.is_some()
    }

    pub fn missing_cli_message(&self) -> String {
        format!("{} cli tool not found - check install in $PATH", self.label)
    }
}

struct BasicClient;

impl Client for BasicClient {
    fn list_threads(&mut self, _archived: bool) -> Result<Vec<Thread>> {
        Ok(Vec::new())
    }

    fn read_messages(&mut self, _thread_id: &str, _turns: usize) -> Result<Vec<Message>> {
        Ok(Vec::new())
    }
}

pub fn command_path(command: &str) -> Option<String> {
    let output = std::process::Command::new("zsh")
        .args([
            "-lc",
            &format!(
                "command -v {}",
                shell_escape::unix::escape(std::borrow::Cow::Borrowed(command))
            ),
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .and_then(|stdout| stdout.lines().next().map(str::trim).map(str::to_owned))
        .filter(|path| !path.is_empty())
}
