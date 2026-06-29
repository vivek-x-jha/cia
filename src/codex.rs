use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::agent::{Message, Thread};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListResponse {
    data: Vec<Thread>,
    next_cursor: Option<String>,
}

pub struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    pub fn start(command: &str) -> Result<Self> {
        let mut child = Command::new(command)
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to start `{command} app-server`"))?;
        let stdin = child
            .stdin
            .take()
            .context("Codex app-server stdin unavailable")?;
        let stdout = BufReader::new(
            child
                .stdout
                .take()
                .context("Codex app-server stdout unavailable")?,
        );
        let mut client = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        client.initialize()?;
        Ok(client)
    }

    fn initialize(&mut self) -> Result<()> {
        self.request(
            "initialize",
            json!({"clientInfo":{"name":"cia","title":"CIA","version":env!("CARGO_PKG_VERSION")}}),
        )?;
        self.send_notification("initialized", json!({}))
    }

    pub(crate) fn list_threads_inner(&mut self, archived: bool) -> Result<Vec<Thread>> {
        let mut threads = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let result = self.request(
                "thread/list",
                json!({
                    "archived": archived,
                    "cursor": cursor,
                    "limit": 100,
                    "sortKey": "recency_at",
                    "sortDirection": "desc"
                }),
            )?;
            let page: ListResponse = serde_json::from_value(result)
                .context("Codex returned an incompatible thread/list response")?;
            threads.extend(page.data.into_iter().map(|mut thread| {
                thread.archived = archived;
                thread
            }));
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        Ok(threads)
    }

    pub(crate) fn read_messages_inner(
        &mut self,
        thread_id: &str,
        turns: usize,
    ) -> Result<Vec<Message>> {
        let result = self.request(
            "thread/read",
            json!({"threadId": thread_id, "includeTurns": true}),
        )?;
        Ok(extract_messages(&result, turns))
    }

    pub(crate) fn delete_thread_inner(&mut self, thread_id: &str) -> Result<()> {
        match self.request("thread/delete", json!({"threadId": thread_id})) {
            Ok(_) => Ok(()),
            Err(error) if error.to_string().contains("no rollout found") => {
                self.request("thread/unarchive", json!({"threadId": thread_id}))?;
                self.request("thread/delete", json!({"threadId": thread_id}))?;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({"method": method, "id": id, "params": params});
        writeln!(self.stdin, "{}", serde_json::to_string(&request)?)?;
        self.stdin.flush()?;

        let mut line = String::new();
        loop {
            line.clear();
            if self.stdout.read_line(&mut line)? == 0 {
                return Err(anyhow!("Codex app-server exited while handling {method}"));
            }
            let response: Value = serde_json::from_str(line.trim())
                .with_context(|| format!("invalid JSON from Codex app-server: {}", line.trim()))?;
            if response.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = response.get("error") {
                return Err(anyhow!("Codex {method} failed: {error}"));
            }
            return response
                .get("result")
                .cloned()
                .context("Codex response omitted result");
        }
    }

    fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        let notification = json!({"method": method, "params": params});
        writeln!(self.stdin, "{}", serde_json::to_string(&notification)?)?;
        self.stdin.flush()?;
        Ok(())
    }
}

impl crate::agent::Client for Client {
    fn list_threads(&mut self, archived: bool) -> Result<Vec<Thread>> {
        self.list_threads_inner(archived)
    }

    fn read_messages(&mut self, thread_id: &str, turns: usize) -> Result<Vec<Message>> {
        self.read_messages_inner(thread_id, turns)
    }

    fn delete_thread(&mut self, thread_id: &str) -> Result<()> {
        self.delete_thread_inner(thread_id)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stop();
    }
}

fn extract_messages(result: &Value, turn_limit: usize) -> Vec<Message> {
    let turns = result
        .pointer("/thread/turns")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let start = turns.len().saturating_sub(turn_limit);
    let mut messages = Vec::new();
    for turn in &turns[start..] {
        for item in turn
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
            let role = match kind {
                "userMessage" => "You",
                "agentMessage" => "Codex",
                _ => continue,
            };
            if let Some(text) = message_text(item) {
                if !text.trim().is_empty() {
                    messages.push(Message {
                        role: role.into(),
                        text,
                    });
                }
            }
        }
    }
    messages
}

fn message_text(item: &Value) -> Option<String> {
    if let Some(text) = item.get("text").and_then(Value::as_str) {
        return Some(text.to_owned());
    }
    let content = item.get("content")?.as_array()?;
    let texts: Vec<&str> = content
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn extracts_only_recent_user_and_agent_messages() {
        let value = json!({"thread":{"turns":[
            {"items":[{"type":"userMessage","content":[{"type":"text","text":"old"}]}]},
            {"items":[{"type":"userMessage","content":[{"type":"text","text":"question"}]},{"type":"commandExecution","command":"pwd"},{"type":"agentMessage","text":"answer"}]}
        ]}});
        let messages = extract_messages(&value, 1);
        assert_eq!(
            messages,
            vec![
                Message {
                    role: "You".into(),
                    text: "question".into()
                },
                Message {
                    role: "Codex".into(),
                    text: "answer".into()
                }
            ]
        );
    }

    #[test]
    fn thread_title_falls_back_to_preview() {
        let thread: Thread = serde_json::from_value(json!({
            "id":"1","name":null,"preview":"First line\nSecond","cwd":"/tmp","createdAt":1,
            "updatedAt":2,"recencyAt":null,"source":"cli","gitInfo":null,"unknown":"ok"
        }))
        .unwrap();
        assert_eq!(thread.title(), "First line");
    }

    #[test]
    fn speaks_jsonl_to_an_isolated_app_server() {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("fake-codex");
        std::fs::write(
            &script,
            r#"#!/usr/bin/env python3
import json, sys
for line in sys.stdin:
    message = json.loads(line)
    if 'id' not in message:
        continue
    method = message['method']
    if method == 'initialize':
        result = {'userAgent': 'fake'}
    elif method == 'thread/list':
        result = {'data': [{'id':'thread-1','name':'Test','preview':'hello','cwd':'/tmp/project','createdAt':1,'updatedAt':2,'recencyAt':2,'source':'cli','gitInfo':None}], 'nextCursor': None}
    elif method == 'thread/read':
        result = {'thread': {'turns': [{'items': [{'type':'agentMessage','text':'done'}]}]}}
    elif method == 'thread/archive' or method == 'thread/unarchive' or method == 'thread/delete':
        result = {}
    print(json.dumps({'id': message['id'], 'result': result}), flush=True)
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();

        let mut client = Client::start(script.to_str().unwrap()).unwrap();
        assert_eq!(client.list_threads_inner(false).unwrap()[0].id, "thread-1");
        assert_eq!(
            client.read_messages_inner("thread-1", 3).unwrap()[0].text,
            "done"
        );
        client.delete_thread_inner("thread-1").unwrap();
    }
}
