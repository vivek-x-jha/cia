use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::tmux::Window;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mapping {
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
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            last_project: None,
            mappings: Vec::new(),
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
            if window.thread_id.is_none() {
                window.thread_id = self
                    .mappings
                    .iter()
                    .find(|mapping| mapping.matches(window))
                    .map(|mapping| mapping.thread_id.clone());
            }
        }
    }

    pub fn record(&mut self, thread_id: &str, window: &Window) {
        self.mappings.retain(|mapping| {
            mapping.thread_id != thread_id
                && mapping.pane_id.as_deref() != Some(window.pane_id.as_str())
        });
        self.mappings.push(Mapping {
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
        self.cwd == window.cwd
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
            thread_id: None,
            chat_title: None,
        }
    }

    #[test]
    fn restores_only_exact_live_window_mapping() {
        let mut state = State::default();
        state.record("thread", &window());
        let mut windows = vec![window()];
        state.reconcile(&mut windows);
        assert_eq!(windows[0].thread_id.as_deref(), Some("thread"));
    }
}
