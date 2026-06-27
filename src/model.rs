use std::collections::{HashMap, HashSet};

use crate::{
    agent::Thread,
    tmux::{normalized_path, Client as TmuxClient, Window},
};

#[derive(Clone, Debug)]
pub struct Project {
    pub name: String,
    pub cwd: String,
    pub recency: i64,
    pub threads: Vec<Thread>,
    pub agents: Vec<Window>,
}

#[derive(Clone, Debug)]
pub enum Row {
    Agent(Window),
    Thread {
        thread: Thread,
        live: Option<Window>,
    },
}

impl Row {
    pub fn title(&self) -> String {
        match self {
            Self::Agent(window) => {
                let label = window.chat_title.as_deref().unwrap_or(&window.window_name);
                format!("Live agent · {}:{label}", window.session)
            }
            Self::Thread { thread, .. } => thread.title().to_owned(),
        }
    }

    pub fn recency(&self) -> i64 {
        match self {
            Self::Agent(window) => window.session_last_attached,
            Self::Thread { thread, .. } => thread.recency(),
        }
    }

    pub fn is_live(&self) -> bool {
        matches!(self, Self::Agent(_)) || matches!(self, Self::Thread { live: Some(_), .. })
    }
}

pub fn build_projects(
    threads: Vec<Thread>,
    windows: Vec<Window>,
    tmux: &TmuxClient,
) -> Vec<Project> {
    let mut projects: HashMap<String, Project> = HashMap::new();
    for thread in threads {
        let key = normalized_path(&thread.cwd).to_string_lossy().into_owned();
        let project = projects.entry(key.clone()).or_insert_with(|| Project {
            name: project_name(&key),
            cwd: key.clone(),
            recency: 0,
            threads: Vec::new(),
            agents: Vec::new(),
        });
        project.recency = project.recency.max(thread.recency());
        project.threads.push(thread);
    }
    for window in windows.into_iter().filter(|window| tmux.is_agent(window)) {
        let key = normalized_path(&window.cwd).to_string_lossy().into_owned();
        let project = projects.entry(key.clone()).or_insert_with(|| Project {
            name: project_name(&key),
            cwd: key.clone(),
            recency: 0,
            threads: Vec::new(),
            agents: Vec::new(),
        });
        project.recency = project.recency.max(window.session_last_attached);
        project.agents.push(window);
    }
    let mut projects: Vec<Project> = projects.into_values().collect();
    for project in &mut projects {
        project
            .threads
            .sort_by_key(|thread| std::cmp::Reverse(thread.recency()));
    }
    projects.sort_by(|a, b| b.recency.cmp(&a.recency).then_with(|| a.name.cmp(&b.name)));
    projects
}

pub fn rows(project: &Project) -> Vec<Row> {
    let mapped: HashMap<&str, &Window> = project
        .agents
        .iter()
        .filter_map(|window| window.thread_id.as_deref().map(|id| (id, window)))
        .collect();
    let thread_ids: HashSet<&str> = project
        .threads
        .iter()
        .map(|thread| thread.id.as_str())
        .collect();
    let mut named_agents: HashMap<&str, Vec<&Window>> = HashMap::new();
    for window in project
        .agents
        .iter()
        .filter(|window| window.thread_id.is_none())
    {
        if let Some(title) = window.chat_title.as_deref() {
            named_agents.entry(title).or_default().push(window);
        }
    }
    let thread_names: HashSet<&str> = project
        .threads
        .iter()
        .filter_map(|thread| thread.name.as_deref())
        .collect();
    let mut result: Vec<Row> = project
        .agents
        .iter()
        .filter(|window| {
            window.thread_id.as_deref().map_or_else(
                || {
                    window
                        .chat_title
                        .as_deref()
                        .is_none_or(|title| !thread_names.contains(title))
                },
                |id| !thread_ids.contains(id),
            )
        })
        .cloned()
        .map(Row::Agent)
        .collect();
    result.extend(project.threads.iter().cloned().map(|thread| {
        let live = mapped
            .get(thread.id.as_str())
            .map(|window| (*window).clone())
            .or_else(|| {
                thread
                    .name
                    .as_deref()
                    .and_then(|name| named_agents.get(name))
                    .filter(|windows| windows.len() == 1)
                    .map(|windows| windows[0].clone())
            });
        Row::Thread { thread, live }
    }));
    result.sort_by_key(|row| {
        (
            std::cmp::Reverse(row.is_live()),
            std::cmp::Reverse(row.recency()),
        )
    });
    result
}

fn project_name(cwd: &str) -> String {
    std::path::Path::new(cwd)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(cwd)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TmuxConfig;
    use serde_json::json;

    fn thread(id: &str, cwd: &str, updated: i64) -> Thread {
        serde_json::from_value(json!({"id":id,"name":id,"preview":"","cwd":cwd,"createdAt":1,"updatedAt":updated,"recencyAt":null,"source":"cli","gitInfo":null})).unwrap()
    }

    #[test]
    fn groups_threads_and_agents_without_guessing() {
        let tmux = TmuxClient::new(TmuxConfig::default());
        let agent = Window {
            session: "repo".into(),
            session_last_attached: 3,
            window_id: "@1".into(),
            window_name: "agents".into(),
            pane_id: "%1".into(),
            pane_pid: 1,
            command: "codex".into(),
            cwd: "/tmp/repo".into(),
            harness_id: None,
            thread_id: None,
            chat_title: None,
        };
        let projects = build_projects(vec![thread("chat", "/tmp/repo", 2)], vec![agent], &tmux);
        let result = rows(&projects[0]);
        assert!(matches!(result[0], Row::Agent(_)));
        assert!(matches!(result[1], Row::Thread { live: None, .. }));
    }

    #[test]
    fn maps_cia_named_panes_without_a_thread_id() {
        let tmux = TmuxClient::new(TmuxConfig::default());
        let agent = Window {
            session: "repo".into(),
            session_last_attached: 3,
            window_id: "@1".into(),
            window_name: "agents".into(),
            pane_id: "%1".into(),
            pane_pid: 1,
            command: "cia".into(),
            cwd: "/tmp/repo".into(),
            harness_id: Some(crate::agent::DEFAULT_HARNESS_ID.into()),
            thread_id: None,
            chat_title: Some("chat".into()),
        };
        let projects = build_projects(vec![thread("chat", "/tmp/repo", 2)], vec![agent], &tmux);
        let result = rows(&projects[0]);
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], Row::Thread { live: Some(_), .. }));
    }
}
