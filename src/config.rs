use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub codex: CodexConfig,
    pub tmux: TmuxConfig,
    pub ui: UiConfig,
    pub theme: ThemeConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CodexConfig {
    pub command: String,
    pub transcript_turns: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TmuxConfig {
    pub command: String,
    pub agent_window_names: Vec<String>,
    pub new_window_prefix: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    pub archived_default: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeConfig {
    pub background: String,
    pub surface: String,
    pub foreground: String,
    pub muted: String,
    pub accent: String,
    pub selected: String,
    pub success: String,
    pub warning: String,
    pub error: String,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            command: "codex".into(),
            transcript_turns: 3,
        }
    }
}

impl Default for TmuxConfig {
    fn default() -> Self {
        Self {
            command: "tmux".into(),
            agent_window_names: vec!["agents".into()],
            new_window_prefix: "agent:".into(),
        }
    }
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            background: "#101218".into(),
            surface: "#1b1e28".into(),
            foreground: "#e6e6e6".into(),
            muted: "#747b8c".into(),
            accent: "#a8c7fa".into(),
            selected: "#30364a".into(),
            success: "#9bd5a5".into(),
            warning: "#e5c07b".into(),
            error: "#e06c75".into(),
        }
    }
}

pub fn config_path() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"))
        .join("cia/config.toml")
}

pub fn state_path() -> PathBuf {
    env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".local/state"))
        .join("cia/state.json")
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read configuration {}", path.display()))?;
        toml::from_str(&source).with_context(|| format!("invalid configuration {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_partial_configuration_with_defaults() {
        let cfg: Config = toml::from_str("[ui]\narchived_default = true\n").unwrap();
        assert!(cfg.ui.archived_default);
        assert_eq!(cfg.codex.command, "codex");
    }

    #[test]
    fn rejects_unknown_configuration_keys() {
        let error = toml::from_str::<Config>("[ui]\nunknown = true\n").unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }
}
