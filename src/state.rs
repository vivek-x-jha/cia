use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{agent::DEFAULT_HARNESS_ID, tmux::Window};

fn default_harness_id() -> String {
    DEFAULT_HARNESS_ID.into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mapping {
    #[serde(default = "default_harness_id")]
    pub harness_id: String,
    pub thread_id: String,
    pub window_id: String,
    #[serde(default)]
    pub pane_id: Option<String>,
    pub session: String,
    pub window_name: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub last_project: Option<String>,
    pub mappings: Vec<Mapping>,
    #[serde(default)]
    pub archived_threads: Vec<ArchivedThread>,
    #[serde(default)]
    pub hidden_threads: Vec<ArchivedThread>,
    #[serde(default)]
    pub project_paths: Vec<String>,
    #[serde(default)]
    pub hidden_project_paths: Vec<String>,
    #[serde(default)]
    pub deleted_project_paths: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchivedThread {
    #[serde(default = "default_harness_id")]
    pub harness_id: String,
    pub thread_id: String,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            last_project: None,
            mappings: Vec::new(),
            archived_threads: Vec::new(),
            hidden_threads: Vec::new(),
            project_paths: Vec::new(),
            hidden_project_paths: Vec::new(),
            deleted_project_paths: Vec::new(),
        }
    }
}

impl State {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let source = fs::read_to_string(path)?;
        let state: Self = serde_json::from_str(&source)?;
        if state.version != 1 {
            anyhow::bail!(
                "unsupported CIA state version {} in {}",
                state.version,
                path.display()
            );
        }
        Ok(state)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .context("CIA state path has no parent directory")?;
        fs::create_dir_all(parent)?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        fs::rename(temporary, path)?;
        Ok(())
    }

    pub fn reconcile(&mut self, windows: &mut [Window]) {
        self.mappings
            .retain(|mapping| windows.iter().any(|window| mapping.matches(window)));
        for window in windows {
            if window.thread_id.is_some() && window.harness_id.is_none() {
                window.harness_id = Some(DEFAULT_HARNESS_ID.into());
            }
            if window.thread_id.is_none() {
                if let Some(mapping) = self.mappings.iter().find(|mapping| mapping.matches(window))
                {
                    window.harness_id = Some(mapping.harness_id.clone());
                    window.thread_id = Some(mapping.thread_id.clone());
                }
            }
        }
    }

    pub fn is_archived(&self, harness_id: &str, thread_id: &str) -> bool {
        self.archived_threads
            .iter()
            .any(|thread| thread.harness_id == harness_id && thread.thread_id == thread_id)
    }

    pub fn set_archived(&mut self, harness_id: &str, thread_id: &str, archived: bool) {
        self.archived_threads
            .retain(|thread| !(thread.harness_id == harness_id && thread.thread_id == thread_id));
        if archived {
            self.archived_threads.push(ArchivedThread {
                harness_id: harness_id.into(),
                thread_id: thread_id.into(),
            });
        }
    }

    pub fn is_hidden(&self, harness_id: &str, thread_id: &str) -> bool {
        self.hidden_threads
            .iter()
            .any(|thread| thread.harness_id == harness_id && thread.thread_id == thread_id)
    }

    pub fn set_hidden(&mut self, harness_id: &str, thread_id: &str, hidden: bool) {
        self.hidden_threads
            .retain(|thread| !(thread.harness_id == harness_id && thread.thread_id == thread_id));
        if hidden {
            self.hidden_threads.push(ArchivedThread {
                harness_id: harness_id.into(),
                thread_id: thread_id.into(),
            });
        }
    }

    pub fn add_project_path(&mut self, cwd: String) {
        self.hidden_project_paths.retain(|path| path != &cwd);
        self.deleted_project_paths.retain(|path| path != &cwd);
        if !self.project_paths.iter().any(|path| path == &cwd) {
            self.project_paths.push(cwd);
        }
    }

    #[allow(dead_code)]
    pub fn hide_project_path(&mut self, cwd: &str) {
        self.project_paths.retain(|path| path != cwd);
        if !self.hidden_project_paths.iter().any(|path| path == cwd) {
            self.hidden_project_paths.push(cwd.to_owned());
        }
        if self.last_project.as_deref() == Some(cwd) {
            self.last_project = None;
        }
    }

    #[allow(dead_code)]
    pub fn unhide_project_path(&mut self, cwd: &str) {
        self.hidden_project_paths.retain(|path| path != cwd);
        self.add_project_path(cwd.to_owned());
    }

    pub fn delete_project_path(&mut self, cwd: &str) {
        self.project_paths.retain(|path| path != cwd);
        self.hidden_project_paths.retain(|path| path != cwd);
        if !self.deleted_project_paths.iter().any(|path| path == cwd) {
            self.deleted_project_paths.push(cwd.to_owned());
        }
        if self.last_project.as_deref() == Some(cwd) {
            self.last_project = None;
        }
    }

    pub fn is_project_hidden(&self, cwd: &str) -> bool {
        self.hidden_project_paths.iter().any(|path| path == cwd)
    }

    pub fn is_project_deleted(&self, cwd: &str) -> bool {
        self.deleted_project_paths.iter().any(|path| path == cwd)
    }

    pub fn is_project_suppressed(&self, cwd: &str) -> bool {
        self.is_project_hidden(cwd) || self.is_project_deleted(cwd)
    }

    pub fn record(&mut self, harness_id: &str, thread_id: &str, window: &Window) {
        self.mappings.retain(|mapping| {
            !(mapping.harness_id == harness_id && mapping.thread_id == thread_id)
                && mapping.pane_id.as_deref() != Some(window.pane_id.as_str())
        });
        self.mappings.push(Mapping {
            harness_id: harness_id.into(),
            thread_id: thread_id.into(),
            window_id: window.window_id.clone(),
            pane_id: Some(window.pane_id.clone()),
            session: window.session.clone(),
            window_name: window.window_name.clone(),
            cwd: window.cwd.clone(),
        });
    }
}

impl Mapping {
    fn matches(&self, window: &Window) -> bool {
        let harness_matches = window
            .harness_id
            .as_deref()
            .is_none_or(|harness_id| harness_id == self.harness_id);
        harness_matches
            && self.cwd == window.cwd
            && self
                .pane_id
                .as_deref()
                .map_or(self.window_id == window.window_id, |pane_id| {
                    pane_id == window.pane_id
                })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> Window {
        Window {
            session: "repo".into(),
            session_last_attached: 0,
            window_id: "@1".into(),
            window_name: "agent".into(),
            pane_id: "%1".into(),
            pane_pid: 1,
            command: "codex".into(),
            cwd: "/repo".into(),
            harness_id: None,
            thread_id: None,
            chat_title: None,
        }
    }

    #[test]
    fn restores_only_exact_live_window_mapping() {
        let mut state = State::default();
        state.record(DEFAULT_HARNESS_ID, "thread", &window());
        let mut windows = vec![window()];
        state.reconcile(&mut windows);
        assert_eq!(windows[0].harness_id.as_deref(), Some(DEFAULT_HARNESS_ID));
        assert_eq!(windows[0].thread_id.as_deref(), Some("thread"));
    }

    #[test]
    fn unhide_moves_project_back_to_visible_paths() {
        let mut state = State::default();
        state.hide_project_path("/repo");
        state.unhide_project_path("/repo");
        assert_eq!(state.hidden_project_paths, Vec::<String>::new());
        assert_eq!(state.project_paths, vec!["/repo".to_owned()]);
    }

    #[test]
    fn delete_suppresses_without_adding_to_hidden_list() {
        let mut state = State::default();
        state.add_project_path("/repo".into());
        state.delete_project_path("/repo");
        assert_eq!(state.project_paths, Vec::<String>::new());
        assert_eq!(state.hidden_project_paths, Vec::<String>::new());
        assert_eq!(state.deleted_project_paths, vec!["/repo".to_owned()]);
        assert!(state.is_project_suppressed("/repo"));
    }
}
