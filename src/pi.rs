use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::agent::{Message, Thread, PI_HARNESS_ID, PI_HARNESS_LABEL};

pub struct Client {
    session_dir: PathBuf,
}

impl Client {
    pub fn new(session_dir: Option<String>) -> Self {
        Self {
            session_dir: session_dir
                .map(PathBuf::from)
                .unwrap_or_else(default_session_dir),
        }
    }
}

impl crate::agent::Client for Client {
    fn list_threads(&mut self, archived: bool) -> Result<Vec<Thread>> {
        let mut threads = Vec::new();
        for path in session_files(&self.session_dir)? {
            if is_archived_path(&self.session_dir, &path) != archived {
                continue;
            }
            if let Some(mut thread) = parse_thread(&path)? {
                thread.archived = archived;
                threads.push(thread);
            }
        }
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.recency()));
        Ok(threads)
    }

    fn read_messages(&mut self, thread_id: &str, turns: usize) -> Result<Vec<Message>> {
        for path in session_files(&self.session_dir)? {
            let session = parse_session(&path)?;
            if session.id.as_deref() == Some(thread_id) {
                return Ok(session
                    .messages
                    .into_iter()
                    .rev()
                    .filter(|message| message.role == "You" || message.role == PI_HARNESS_LABEL)
                    .take(turns.saturating_mul(2))
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect());
            }
        }
        Ok(Vec::new())
    }

    fn set_archived(&mut self, thread_id: &str, archived: bool) -> Result<()> {
        let Some(path) = find_session_path(&self.session_dir, thread_id)? else {
            anyhow::bail!("Pi session not found for {thread_id}");
        };
        if is_archived_path(&self.session_dir, &path) == archived {
            return Ok(());
        }
        let target_dir = if archived {
            self.session_dir.join("archived")
        } else {
            self.session_dir.clone()
        };
        fs::create_dir_all(&target_dir)?;
        let target = target_dir.join(
            path.file_name()
                .context("Pi session path has no file name")?,
        );
        fs::rename(&path, target)?;
        Ok(())
    }

    fn delete_thread(&mut self, thread_id: &str) -> Result<()> {
        let Some(path) = find_session_path(&self.session_dir, thread_id)? else {
            return Ok(());
        };
        fs::remove_file(path)?;
        Ok(())
    }
}

#[derive(Default)]
struct Session {
    id: Option<String>,
    name: Option<String>,
    cwd: Option<String>,
    created_at: i64,
    updated_at: i64,
    first_user_text: Option<String>,
    preview: Option<String>,
    messages: Vec<Message>,
}

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: String,
    timestamp: Option<String>,
    id: Option<String>,
    cwd: Option<String>,
    name: Option<String>,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    message: Option<PiMessage>,
}

#[derive(Deserialize)]
struct PiMessage {
    role: String,
    content: Option<Value>,
}

fn default_session_dir() -> PathBuf {
    if let Some(path) = env::var_os("PI_CODING_AGENT_SESSION_DIR") {
        return PathBuf::from(path);
    }
    if let Some(path) = env::var_os("PI_CODING_AGENT_DIR") {
        return PathBuf::from(path).join("sessions");
    }
    home_dir().join(".pi/agent/sessions")
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn session_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_session_files(root, &mut files)?;
    Ok(files)
}

fn collect_session_files(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_session_files(&path, files)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    Ok(())
}

fn parse_thread(path: &Path) -> Result<Option<Thread>> {
    let session = parse_session(path)?;
    let Some(id) = session.id else {
        return Ok(None);
    };
    let cwd = session.cwd.unwrap_or_else(|| ".".into());
    let name = session.name.or(session.first_user_text);
    Ok(Some(Thread {
        harness_id: PI_HARNESS_ID.into(),
        id,
        name,
        preview: session.preview.unwrap_or_default(),
        cwd,
        created_at: session.created_at,
        updated_at: session.updated_at,
        recency_at: Some(session.updated_at),
        source: json!(PI_HARNESS_ID),
        git_info: None,
        archived: false,
    }))
}

fn is_archived_path(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).ok().is_some_and(|relative| {
        relative
            .components()
            .next()
            .is_some_and(|component| component.as_os_str() == "archived")
    })
}

fn find_session_path(root: &Path, thread_id: &str) -> Result<Option<PathBuf>> {
    for path in session_files(root)? {
        if parse_session(&path)?.id.as_deref() == Some(thread_id) {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn parse_session(path: &Path) -> Result<Session> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("failed to read Pi session {}", path.display()))?;
    let fallback_time = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default();
    let mut session = Session {
        created_at: fallback_time,
        updated_at: fallback_time,
        ..Session::default()
    };
    for line in source.lines().filter(|line| !line.trim().is_empty()) {
        let record: Record = serde_json::from_str(line)
            .with_context(|| format!("invalid Pi session JSON in {}", path.display()))?;
        let timestamp = record
            .timestamp
            .as_deref()
            .and_then(parse_timestamp)
            .unwrap_or(fallback_time);
        session.created_at = if session.created_at == 0 {
            timestamp
        } else {
            session.created_at.min(timestamp)
        };
        session.updated_at = session.updated_at.max(timestamp);
        match record.kind.as_str() {
            "session" => {
                session.id = record.id.or(session.id);
                session.cwd = record.cwd.or(session.cwd);
                session.name = record.name.or(record.display_name).or(session.name);
            }
            "message" => {
                if let Some(message) = record.message {
                    if let Some(text) = message_text(message.content.as_ref()) {
                        if text.trim().is_empty() {
                            continue;
                        }
                        match message.role.as_str() {
                            "user" => {
                                session
                                    .first_user_text
                                    .get_or_insert_with(|| first_line(&text));
                                session.preview = Some(text.clone());
                                session.messages.push(Message {
                                    role: "You".into(),
                                    text,
                                });
                            }
                            "assistant" => {
                                session.preview = Some(text.clone());
                                session.messages.push(Message {
                                    role: PI_HARNESS_LABEL.into(),
                                    text,
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
            "session_info" | "session_name" | "name" => {
                session.name = record.name.or(record.display_name).or(session.name);
            }
            _ => {}
        }
    }
    Ok(session)
}

fn parse_timestamp(value: &str) -> Option<i64> {
    OffsetDateTime::parse(value, &Rfc3339)
        .ok()
        .map(|time| time.unix_timestamp())
}

fn message_text(content: Option<&Value>) -> Option<String> {
    let content = content?;
    if let Some(text) = content.as_str() {
        return Some(text.to_owned());
    }
    let parts = content.as_array()?;
    let texts: Vec<&str> = parts
        .iter()
        .filter_map(|part| {
            (part.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| part.get("text").and_then(Value::as_str))
                .flatten()
        })
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

fn first_line(value: &str) -> String {
    value
        .lines()
        .next()
        .filter(|line| !line.trim().is_empty())
        .unwrap_or(value)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pi_session_jsonl() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        fs::write(
            &path,
            r#"{"type":"session","version":3,"id":"pi-1","timestamp":"2026-06-27T20:26:07.719Z","cwd":"/tmp/repo"}
{"type":"message","timestamp":"2026-06-27T20:29:49.237Z","message":{"role":"user","content":[{"type":"text","text":"What files are here?\nSecond"}]}}
{"type":"message","timestamp":"2026-06-27T20:29:51.356Z","message":{"role":"assistant","content":[{"type":"toolCall","name":"bash"}]}}
{"type":"message","timestamp":"2026-06-27T20:29:52.979Z","message":{"role":"assistant","content":[{"type":"text","text":"Files here."}]}}
"#,
        )
        .unwrap();

        let thread = parse_thread(&path).unwrap().unwrap();
        assert_eq!(thread.harness_id, PI_HARNESS_ID);
        assert_eq!(thread.id, "pi-1");
        assert_eq!(thread.cwd, "/tmp/repo");
        assert_eq!(thread.title(), "What files are here?");
        assert_eq!(thread.preview, "Files here.");

        let session = parse_session(&path).unwrap();
        assert_eq!(
            session.messages,
            vec![
                Message {
                    role: "You".into(),
                    text: "What files are here?\nSecond".into()
                },
                Message {
                    role: "Pi".into(),
                    text: "Files here.".into()
                }
            ]
        );
    }

    #[test]
    fn favors_pi_session_info_name_over_first_user_message() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        fs::write(
            &path,
            r#"{"type":"session","version":3,"id":"pi-1","timestamp":"2026-06-27T20:26:07.719Z","cwd":"/tmp/repo"}
{"type":"session_info","id":"event-1","parentId":null,"timestamp":"2026-06-27T20:26:07.719Z","name":"clean-chat-title"}
{"type":"message","timestamp":"2026-06-27T20:29:49.237Z","message":{"role":"user","content":[{"type":"text","text":"This is a very long first prompt that should not be used when a session name exists."}]}}
"#,
        )
        .unwrap();

        let thread = parse_thread(&path).unwrap().unwrap();
        assert_eq!(thread.id, "pi-1");
        assert_eq!(thread.name.as_deref(), Some("clean-chat-title"));
        assert_eq!(thread.title(), "clean-chat-title");
    }
}
