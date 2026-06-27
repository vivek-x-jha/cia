use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use crate::{codex, config::Config};

pub const DEFAULT_HARNESS_ID: &str = "codex";
pub const DEFAULT_HARNESS_LABEL: &str = "Codex";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    pub name: Option<String>,
    pub preview: String,
    pub cwd: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub recency_at: Option<i64>,
    pub source: Value,
    pub git_info: Option<GitInfo>,
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
                .unwrap_or_else(|| DEFAULT_HARNESS_ID.into()),
            _ => DEFAULT_HARNESS_ID.into(),
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
}

pub struct Harness {
    pub id: String,
    pub label: String,
    pub command: String,
    client: Box<dyn Client>,
}

impl Harness {
    pub fn start(config: &Config) -> Result<Self> {
        Ok(Self {
            id: DEFAULT_HARNESS_ID.into(),
            label: DEFAULT_HARNESS_LABEL.into(),
            command: config.codex.command.clone(),
            client: Box::new(codex::Client::start(&config.codex.command)?),
        })
    }

    pub fn list_threads(&mut self, archived: bool) -> Result<Vec<Thread>> {
        self.client.list_threads(archived)
    }

    pub fn read_messages(&mut self, thread_id: &str, turns: usize) -> Result<Vec<Message>> {
        self.client.read_messages(thread_id, turns)
    }
}
