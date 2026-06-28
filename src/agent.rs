use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use crate::{codex, config::Config, pi};

pub const CODEX_HARNESS_ID: &str = "codex";
pub const CODEX_HARNESS_LABEL: &str = "Codex";
pub const PI_HARNESS_ID: &str = "pi";
pub const PI_HARNESS_LABEL: &str = "Pi";
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub role: String,
    pub text: String,
}

pub trait Client {
    fn list_threads(&mut self, archived: bool) -> Result<Vec<Thread>>;
    fn read_messages(&mut self, thread_id: &str, turns: usize) -> Result<Vec<Message>>;
    fn set_archived(&mut self, thread_id: &str, archived: bool) -> Result<()>;
    fn delete_thread(&mut self, thread_id: &str) -> Result<()>;
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
}

impl Harness {
    pub fn start_all(config: &Config) -> Vec<Result<Self>> {
        let mut harnesses = vec![Self::start_codex(config)];
        if config
            .pi
            .enabled
            .unwrap_or_else(|| command_exists(&config.pi.command))
        {
            harnesses.push(Self::start_pi(config));
        }
        harnesses
    }

    pub fn start_codex(config: &Config) -> Result<Self> {
        Ok(Self {
            id: CODEX_HARNESS_ID.into(),
            label: CODEX_HARNESS_LABEL.into(),
            marker: "Codex".into(),
            command: config.codex.command.clone(),
            client: Box::new(codex::Client::start(&config.codex.command)?),
        })
    }

    pub fn start_pi(config: &Config) -> Result<Self> {
        Ok(Self {
            id: PI_HARNESS_ID.into(),
            label: PI_HARNESS_LABEL.into(),
            marker: "π".into(),
            command: config.pi.command.clone(),
            client: Box::new(pi::Client::new(config.pi.session_dir.clone())),
        })
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

    pub fn set_archived(&mut self, thread_id: &str, archived: bool) -> Result<()> {
        self.client.set_archived(thread_id, archived)
    }

    pub fn delete_thread(&mut self, thread_id: &str) -> Result<()> {
        self.client.delete_thread(thread_id)
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
