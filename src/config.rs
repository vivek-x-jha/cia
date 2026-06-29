use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

const DEFAULT_UI_ARCHIVE_ICON: &str = "";
const DEFAULT_UI_DEFAULT_HARNESS: &str = "pi";
const DEFAULT_NEW_CHAT_CURSOR_COLOR: &str = "$BLACK_HEX";
const DEFAULT_NEW_CHAT_OPENCODE_COLOR: &str = "$WHITE_HEX";

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub codex: CodexConfig,
    pub pi: PiConfig,
    pub claude: ClaudeConfig,
    pub cursor: CursorConfig,
    pub opencode: OpencodeConfig,
    pub tmux: TmuxConfig,
    pub ui: UiConfig,
    pub theme: ThemeConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CodexConfig {
    pub command: String,
    pub icon: String,
    pub label: String,
    pub transcript_turns: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PiConfig {
    pub command: String,
    pub icon: String,
    pub label: String,
    pub session_dir: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaudeConfig {
    pub command: String,
    pub icon: String,
    pub label: String,
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CursorConfig {
    pub command: String,
    pub icon: String,
    pub label: String,
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpencodeConfig {
    pub command: String,
    pub icon: String,
    pub label: String,
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TmuxConfig {
    pub command: String,
    pub agent_commands: Vec<String>,
    pub agent_window_names: Vec<String>,
    pub new_window_prefix: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    pub archived_default: bool,
    pub archive_icon: String,
    pub default_harness: String,
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
    pub title_focused: String,
    pub title_unfocused: String,
    pub border_focused: String,
    pub border_unfocused: String,
    pub status_projects: String,
    pub status_threads: String,
    pub status_open: String,
    pub status_new: String,
    pub status_new_chat: String,
    pub status_search: String,
    pub status_archive: String,
    pub status_archive_action: String,
    pub status_unarchive: String,
    pub status_delete: String,
    pub archive_icon: String,
    pub status_help: String,
    pub preview_user: String,
    pub preview_codex: String,
    pub preview_pi: String,
    pub preview_text: String,
    pub preview_title: String,
    pub new_chat_unfocused: String,
    pub new_chat_pi: String,
    pub new_chat_claude: String,
    pub new_chat_codex: String,
    pub new_chat_cursor: String,
    pub new_chat_opencode: String,
    pub new_chat_path: String,
    pub new_chat_executable: String,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            command: "codex".into(),
            icon: "󱙺".into(),
            label: "Codex".into(),
            transcript_turns: 3,
        }
    }
}

impl Default for PiConfig {
    fn default() -> Self {
        Self {
            command: "pi".into(),
            icon: "π".into(),
            label: "Pi".into(),
            session_dir: None,
            enabled: None,
        }
    }
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            command: "claude".into(),
            icon: "".into(),
            label: "Claude Code".into(),
            enabled: None,
        }
    }
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            command: "cursor".into(),
            icon: "󰋙".into(),
            label: "Cursor".into(),
            enabled: None,
        }
    }
}

impl Default for OpencodeConfig {
    fn default() -> Self {
        Self {
            command: "opencode".into(),
            icon: "".into(),
            label: "OpenCode".into(),
            enabled: None,
        }
    }
}

impl Default for TmuxConfig {
    fn default() -> Self {
        Self {
            command: "tmux".into(),
            agent_commands: vec![
                "pi".into(),
                "claude".into(),
                "codex".into(),
                "cursor".into(),
                "opencode".into(),
            ],
            agent_window_names: vec!["agents".into()],
            new_window_prefix: "agent:".into(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            archived_default: false,
            archive_icon: DEFAULT_UI_ARCHIVE_ICON.into(),
            default_harness: DEFAULT_UI_DEFAULT_HARNESS.into(),
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
            error: "$BRIGHTRED_HEX".into(),
            title_focused: "#d2fd9d".into(),
            title_unfocused: "#5c617d".into(),
            border_focused: "#000000".into(),
            border_unfocused: "#5c617d".into(),
            status_projects: "#e6e6e6".into(),
            status_threads: "#000000".into(),
            status_open: "#80d7fe".into(),
            status_new: "#80d7fe".into(),
            status_new_chat: "#9bd5a5".into(),
            status_search: "#0000ff".into(),
            status_archive: "#e06c75".into(),
            status_archive_action: "#e06c75".into(),
            status_unarchive: "#c678dd".into(),
            status_delete: "#e06c75".into(),
            archive_icon: "#ff0000".into(),
            status_help: "#e5c07b".into(),
            preview_user: "#0000ff".into(),
            preview_codex: "#00ffff".into(),
            preview_pi: "$MAGENTA_HEX".into(),
            preview_text: "#e6e6e6".into(),
            preview_title: "$CYAN_HEX".into(),
            new_chat_unfocused: "$BRIGHTBLACK_HEX".into(),
            new_chat_pi: "$MAGENTA_HEX".into(),
            new_chat_claude: "$BRIGHTYELLOW_HEX".into(),
            new_chat_codex: "$BRIGHTMAGENTA_HEX".into(),
            new_chat_cursor: DEFAULT_NEW_CHAT_CURSOR_COLOR.into(),
            new_chat_opencode: DEFAULT_NEW_CHAT_OPENCODE_COLOR.into(),
            new_chat_path: "$BLUE_HEX".into(),
            new_chat_executable: "$BRIGHTGREEN_HEX".into(),
        }
    }
}

pub fn config_path() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"))
        .join("cia/config.toml")
}

pub fn state_dir() -> PathBuf {
    env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".local/state"))
        .join("cia")
}

pub fn state_path() -> PathBuf {
    state_dir().join("state.json")
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
            let mut config = Self::default();
            config.expand_env();
            return Ok(config);
        }
        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read configuration {}", path.display()))?;
        let mut config: Self = toml::from_str(&source)
            .with_context(|| format!("invalid configuration {}", path.display()))?;
        config.expand_env();
        Ok(config)
    }

    fn expand_env(&mut self) {
        self.codex.command = expand_env_vars(&self.codex.command);
        self.codex.icon = expand_env_vars(&self.codex.icon);
        self.codex.label = expand_env_vars(&self.codex.label);
        self.pi.command = expand_env_vars(&self.pi.command);
        self.pi.icon = expand_env_vars(&self.pi.icon);
        self.pi.label = expand_env_vars(&self.pi.label);
        self.claude.command = expand_env_vars(&self.claude.command);
        self.claude.icon = expand_env_vars(&self.claude.icon);
        self.claude.label = expand_env_vars(&self.claude.label);
        self.cursor.command = expand_env_vars(&self.cursor.command);
        self.cursor.icon = expand_env_vars(&self.cursor.icon);
        self.cursor.label = expand_env_vars(&self.cursor.label);
        self.opencode.command = expand_env_vars(&self.opencode.command);
        self.opencode.icon = expand_env_vars(&self.opencode.icon);
        self.opencode.label = expand_env_vars(&self.opencode.label);
        self.pi.session_dir = self
            .pi
            .session_dir
            .as_ref()
            .map(|value| expand_env_vars(value));
        self.tmux.command = expand_env_vars(&self.tmux.command);
        self.tmux.agent_commands = self
            .tmux
            .agent_commands
            .iter()
            .map(|value| expand_env_vars(value))
            .collect();
        self.tmux.agent_window_names = self
            .tmux
            .agent_window_names
            .iter()
            .map(|value| expand_env_vars(value))
            .collect();
        self.tmux.new_window_prefix = expand_env_vars(&self.tmux.new_window_prefix);
        self.ui.archive_icon = expand_env_vars(&self.ui.archive_icon);
        self.ui.default_harness = expand_env_vars(&self.ui.default_harness);
        self.theme.expand_env();
    }
}

impl ThemeConfig {
    fn expand_env(&mut self) {
        self.background = expand_env_vars(&self.background);
        self.surface = expand_env_vars(&self.surface);
        self.foreground = expand_env_vars(&self.foreground);
        self.muted = expand_env_vars(&self.muted);
        self.accent = expand_env_vars(&self.accent);
        self.selected = expand_env_vars(&self.selected);
        self.success = expand_env_vars(&self.success);
        self.warning = expand_env_vars(&self.warning);
        self.error = expand_env_vars(&self.error);
        self.title_focused = expand_env_vars(&self.title_focused);
        self.title_unfocused = expand_env_vars(&self.title_unfocused);
        self.border_focused = expand_env_vars(&self.border_focused);
        self.border_unfocused = expand_env_vars(&self.border_unfocused);
        self.status_projects = expand_env_vars(&self.status_projects);
        self.status_threads = expand_env_vars(&self.status_threads);
        self.status_open = expand_env_vars(&self.status_open);
        self.status_new = expand_env_vars(&self.status_new);
        self.status_new_chat = expand_env_vars(&self.status_new_chat);
        self.status_search = expand_env_vars(&self.status_search);
        self.status_archive = expand_env_vars(&self.status_archive);
        self.status_archive_action = expand_env_vars(&self.status_archive_action);
        self.status_unarchive = expand_env_vars(&self.status_unarchive);
        self.status_delete = expand_env_vars(&self.status_delete);
        self.archive_icon = expand_env_vars(&self.archive_icon);
        self.status_help = expand_env_vars(&self.status_help);
        self.preview_user = expand_env_vars(&self.preview_user);
        self.preview_codex = expand_env_vars(&self.preview_codex);
        self.preview_pi = expand_env_vars(&self.preview_pi);
        self.preview_text = expand_env_vars(&self.preview_text);
        self.preview_title = expand_env_vars(&self.preview_title);
        self.new_chat_unfocused = expand_env_vars(&self.new_chat_unfocused);
        self.new_chat_pi = expand_env_vars(&self.new_chat_pi);
        self.new_chat_claude = expand_env_vars(&self.new_chat_claude);
        self.new_chat_codex = expand_env_vars(&self.new_chat_codex);
        self.new_chat_cursor = expand_env_vars(&self.new_chat_cursor);
        self.new_chat_opencode = expand_env_vars(&self.new_chat_opencode);
        self.new_chat_path = expand_env_vars(&self.new_chat_path);
        self.new_chat_executable = expand_env_vars(&self.new_chat_executable);
    }
}

fn expand_env_vars(value: &str) -> String {
    let mut result = String::new();
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '$' {
            result.push(character);
            continue;
        }
        match chars.peek().copied() {
            Some('$') => {
                chars.next();
                result.push('$');
            }
            Some('{') => {
                chars.next();
                let mut name = String::new();
                for next in chars.by_ref() {
                    if next == '}' {
                        break;
                    }
                    name.push(next);
                }
                result.push_str(&env::var(&name).unwrap_or_default());
            }
            Some(next) if next == '_' || next.is_ascii_alphabetic() => {
                let mut name = String::new();
                while let Some(next) = chars.peek().copied() {
                    if next == '_' || next.is_ascii_alphanumeric() {
                        name.push(next);
                        chars.next();
                    } else {
                        break;
                    }
                }
                result.push_str(&env::var(&name).unwrap_or_default());
            }
            _ => result.push('$'),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_partial_configuration_with_defaults() {
        let cfg: Config = toml::from_str("[ui]\narchived_default = true\n").unwrap();
        assert!(cfg.ui.archived_default);
        assert_eq!(cfg.codex.command, "codex");
        assert_eq!(cfg.pi.command, "pi");
        assert_eq!(cfg.claude.command, "claude");
        assert_eq!(cfg.claude.icon, "");
        assert_eq!(cfg.claude.label, "Claude Code");
        assert_eq!(cfg.cursor.command, "cursor");
        assert_eq!(cfg.cursor.icon, "󰋙");
        assert_eq!(cfg.opencode.command, "opencode");
        assert_eq!(cfg.ui.archive_icon, DEFAULT_UI_ARCHIVE_ICON);
        assert_eq!(cfg.ui.default_harness, DEFAULT_UI_DEFAULT_HARNESS);
        assert_eq!(cfg.opencode.icon, "");
        assert_eq!(
            cfg.tmux.agent_commands,
            vec!["pi", "claude", "codex", "cursor", "opencode"]
        );
    }

    #[test]
    fn merges_partial_theme_configuration_with_defaults() {
        let cfg: Config = toml::from_str("[theme]\nstatus_open = \"#112233\"\n").unwrap();
        assert_eq!(cfg.theme.status_open, "#112233");
        assert_eq!(cfg.theme.status_projects, "#e6e6e6");
        assert_eq!(cfg.theme.title_focused, "#d2fd9d");
        assert_eq!(cfg.theme.title_unfocused, "#5c617d");
        assert_eq!(cfg.theme.border_focused, "#000000");
        assert_eq!(cfg.theme.border_unfocused, "#5c617d");
        assert_eq!(cfg.theme.status_new, "#80d7fe");
        assert_eq!(cfg.theme.status_new_chat, "#9bd5a5");
        assert_eq!(cfg.theme.preview_codex, "#00ffff");
        assert_eq!(cfg.theme.preview_pi, "$MAGENTA_HEX");
        assert_eq!(cfg.theme.preview_title, "$CYAN_HEX");
        assert_eq!(cfg.theme.new_chat_unfocused, "$BRIGHTBLACK_HEX");
        assert_eq!(cfg.theme.new_chat_pi, "$MAGENTA_HEX");
        assert_eq!(cfg.theme.new_chat_claude, "$BRIGHTYELLOW_HEX");
        assert_eq!(cfg.theme.new_chat_codex, "$BRIGHTMAGENTA_HEX");
        assert_eq!(cfg.theme.new_chat_cursor, DEFAULT_NEW_CHAT_CURSOR_COLOR);
        assert_eq!(cfg.theme.new_chat_opencode, DEFAULT_NEW_CHAT_OPENCODE_COLOR);
        assert_eq!(cfg.theme.new_chat_path, "$BLUE_HEX");
        assert_eq!(cfg.theme.new_chat_executable, "$BRIGHTGREEN_HEX");
    }

    #[test]
    fn expands_environment_variables_in_config_strings() {
        env::set_var("CIA_TEST_COLOR_HEX", "#123456");
        env::set_var("CIA_TEST_COMMAND", "wrapped-codex");
        let mut cfg: Config = toml::from_str(
            "[codex]\ncommand = \"$CIA_TEST_COMMAND\"\n[theme]\nstatus_open = \"${CIA_TEST_COLOR_HEX}\"\n",
        )
        .unwrap();
        cfg.expand_env();
        assert_eq!(cfg.codex.command, "wrapped-codex");
        assert_eq!(cfg.theme.status_open, "#123456");
    }

    #[test]
    fn rejects_unknown_configuration_keys() {
        let error = toml::from_str::<Config>("[ui]\nunknown = true\n").unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }
}
