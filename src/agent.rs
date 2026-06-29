use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use crate::{codex, config::Config, pi};

pub const CODEX_HARNESS_ID: &str = "codex";
pub const CODEX_HARNESS_LABEL: &str = "Codex";
pub const PI_HARNESS_ID: &str = "pi";
pub const PI_HARNESS_LABEL: &str = "Pi";
pub const CLAUDE_HARNESS_ID: &str = "claude";
pub const CLAUDE_HARNESS_LABEL: &str = "Claude Code";
pub const OPENCODE_HARNESS_ID: &str = "opencode";
pub const OPENCODE_HARNESS_LABEL: &str = "OpenCode";
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
    pub source: Value,
    pub git_info: Option<GitInfo>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub path: Option<String>,
}

fn default_harness_id() -> String {
    DEFAULT_HARNESS_ID.into()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitInfo {
    pub branch: Option<String>,
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

    pub fn source_label(&self) -> String {
        match &self.source {
            Value::String(value) => value.clone(),
            Value::Object(value) => value
                .keys()
                .next()
                .cloned()
                .unwrap_or_else(|| self.harness_id.clone()),
            _ => self.harness_id.clone(),
        }
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
        if config
            .pi
            .enabled
            .unwrap_or_else(|| command_exists(&config.pi.command))
        {
            harnesses.push(Self::start_pi(config));
        }
        harnesses.push(Self::start_codex(config));
        if config
            .claude
            .enabled
            .unwrap_or_else(|| command_exists(&config.claude.command))
        {
            harnesses.push(Ok(Self::basic(
                CLAUDE_HARNESS_ID,
                CLAUDE_HARNESS_LABEL,
                &config.claude.icon,
                &config.claude.command,
            )));
        }
        if config
            .opencode
            .enabled
            .unwrap_or_else(|| command_exists(&config.opencode.command))
        {
            harnesses.push(Ok(Self::basic(
                OPENCODE_HARNESS_ID,
                OPENCODE_HARNESS_LABEL,
                &config.opencode.icon,
                &config.opencode.command,
            )));
        }
        harnesses
    }

    pub fn start_codex(config: &Config) -> Result<Self> {
        Ok(Self {
            id: CODEX_HARNESS_ID.into(),
            label: CODEX_HARNESS_LABEL.into(),
            marker: config.codex.icon.clone(),
            command: config.codex.command.clone(),
            client: Box::new(codex::Client::start(&config.codex.command)?),
        })
    }

    pub fn start_pi(config: &Config) -> Result<Self> {
        Ok(Self {
            id: PI_HARNESS_ID.into(),
            label: PI_HARNESS_LABEL.into(),
            marker: config.pi.icon.clone(),
            command: config.pi.command.clone(),
            client: Box::new(pi::Client::new(config.pi.session_dir.clone())),
        })
    }

    fn basic(id: &str, label: &str, marker: &str, command: &str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            marker: marker.into(),
            command: command.into(),
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

fn command_exists(command: &str) -> bool {
    std::process::Command::new("zsh")
        .args([
            "-lc",
            &format!(
                "command -v {}",
                shell_escape::unix::escape(std::borrow::Cow::Borrowed(command))
            ),
        ])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}
