use std::{
    borrow::Cow,
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};

use crate::config::TmuxConfig;

const SEP: char = '\u{1f}';

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub session: String,
    pub session_last_attached: i64,
    pub window_id: String,
    pub window_name: String,
    pub pane_id: String,
    pub pane_pid: u32,
    pub command: String,
    pub cwd: String,
    pub harness_id: Option<String>,
    pub thread_id: Option<String>,
    pub chat_title: Option<String>,
}

pub struct AgentLaunch<'a> {
    pub inventory: &'a [Window],
    pub cwd: &'a str,
    pub title: &'a str,
    pub harness_id: &'a str,
    pub thread_id: Option<&'a str>,
    pub cia_command: &'a str,
    pub agent_command: &'a str,
    pub session_dir: Option<&'a str>,
}

pub struct Client {
    config: TmuxConfig,
}

impl Client {
    pub fn new(config: TmuxConfig) -> Self {
        Self { config }
    }

    pub fn inventory(&self) -> Result<Vec<Window>> {
        let format = format!(
            "#{{session_name}}{SEP}#{{session_last_attached}}{SEP}#{{window_id}}{SEP}#{{window_name}}{SEP}#{{pane_id}}{SEP}#{{pane_pid}}{SEP}#{{pane_current_command}}{SEP}#{{pane_current_path}}{SEP}#{{@cia_harness}}{SEP}#{{@cia_thread_id}}{SEP}#{{@cia_chat_title}}"
        );
        let output = Command::new(&self.config.command)
            .args(["list-panes", "-a", "-F", &format])
            .output()
            .with_context(|| format!("failed to run {} list-panes", self.config.command))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("no server running") || stderr.contains("failed to connect") {
                return Ok(Vec::new());
            }
            return Err(anyhow!("tmux inventory failed: {}", stderr.trim()));
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(parse_window)
            .collect()
    }

    pub fn is_agent(&self, window: &Window) -> bool {
        self.config
            .agent_commands
            .iter()
            .any(|command| command == &window.command)
            || window.harness_id.is_some()
            || window.thread_id.is_some()
            || window.chat_title.is_some()
            || window
                .window_name
                .starts_with(&self.config.new_window_prefix)
    }

    pub fn open_agent(&self, launch: AgentLaunch<'_>) -> Result<Window> {
        let mut command = format!(
            "{} run-thread --harness-id {}",
            shell(launch.cia_command),
            shell(launch.harness_id)
        );
        if let Some(thread_id) = launch.thread_id {
            command.push_str(&format!(" --thread-id {}", shell(thread_id)));
        }
        command.push_str(&format!(
            " --cwd-hex {} --title-hex {} --agent-command-hex {}",
            crate::runner::encode(launch.cwd),
            crate::runner::encode(launch.title),
            crate::runner::encode(launch.agent_command)
        ));
        if let Some(session_dir) = launch.session_dir {
            command.push_str(&format!(
                " --session-dir-hex {}",
                crate::runner::encode(session_dir)
            ));
        }
        self.open_agent_pane(
            launch.inventory,
            launch.cwd,
            launch.title,
            launch.harness_id,
            launch.thread_id,
            &command,
        )
    }

    fn open_agent_pane(
        &self,
        inventory: &[Window],
        cwd: &str,
        title: &str,
        harness_id: &str,
        thread_id: Option<&str>,
        command: &str,
    ) -> Result<Window> {
        let target_cwd = normalized_path(cwd);
        let session = inventory
            .iter()
            .filter(|window| normalized_path(&window.cwd) == target_cwd)
            .max_by(|a, b| {
                a.session_last_attached
                    .cmp(&b.session_last_attached)
                    .then_with(|| b.session.cmp(&a.session))
            })
            .map(|window| window.session.clone());

        let session_name = session.unwrap_or_else(|| unique_session_name(inventory, cwd));
        let window_name = self
            .config
            .agent_window_names
            .first()
            .map(String::as_str)
            .unwrap_or("agents");

        let existing_agent_window = inventory
            .iter()
            .find(|window| window.session == session_name && window.window_name == window_name);
        let pane_id = if let Some(window) = existing_agent_window {
            self.output(&[
                "split-window",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                &window.window_id,
                "-c",
                cwd,
            ])?
        } else if inventory
            .iter()
            .any(|window| window.session == session_name)
        {
            self.output(&[
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                &session_name,
                "-c",
                cwd,
                "-n",
                window_name,
            ])?
        } else {
            self.output(&[
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-s",
                &session_name,
                "-c",
                cwd,
                "-n",
                window_name,
            ])?
        };

        if let Some(thread_id) = thread_id {
            self.mark_pane(&pane_id, harness_id, thread_id, title)?;
        } else {
            self.mark_title(&pane_id, harness_id, title)?;
        }
        self.run(&["send-keys", "-t", &pane_id, "-l", command])?;
        self.run(&["send-keys", "-t", &pane_id, "Enter"])?;
        let refreshed = self.inventory()?;
        refreshed
            .into_iter()
            .find(|window| window.pane_id == pane_id)
            .context("tmux created the agent pane but it was not discoverable")
    }

    pub fn mark_pane(
        &self,
        pane_id: &str,
        harness_id: &str,
        thread_id: &str,
        title: &str,
    ) -> Result<()> {
        self.run(&[
            "set-option",
            "-p",
            "-t",
            pane_id,
            "@cia_harness",
            harness_id,
        ])?;
        self.run(&[
            "set-option",
            "-p",
            "-t",
            pane_id,
            "@cia_thread_id",
            thread_id,
        ])?;
        self.mark_title(pane_id, harness_id, title)
    }

    pub fn mark_title(&self, pane_id: &str, harness_id: &str, title: &str) -> Result<()> {
        self.run(&[
            "set-option",
            "-p",
            "-t",
            pane_id,
            "@cia_harness",
            harness_id,
        ])?;
        self.run(&["set-option", "-p", "-t", pane_id, "@cia_chat_title", title])?;
        self.run(&["select-pane", "-t", pane_id, "-T", title])
    }

    pub fn rename_active_agent_thread(&self, pane_id: &str, title: &str) -> Result<()> {
        thread::sleep(Duration::from_millis(1_000));
        self.run(&["send-keys", "-t", pane_id, "-l", "/rename"])?;
        self.run(&["send-keys", "-t", pane_id, "Enter"])?;
        thread::sleep(Duration::from_millis(250));
        self.run(&["send-keys", "-t", pane_id, "-l", title])?;
        thread::sleep(Duration::from_millis(150));
        self.run(&["send-keys", "-t", pane_id, "Enter"])
    }

    pub fn switch_to(&self, window: &Window) -> Result<()> {
        if env::var_os("TMUX").is_some() {
            self.run(&["switch-client", "-t", &window.session])?;
            self.run(&["select-window", "-t", &window.window_id])?;
            let zoomed = self.output(&[
                "display-message",
                "-p",
                "-t",
                &window.window_id,
                "#{window_zoomed_flag}",
            ])? == "1";
            if zoomed {
                self.run(&["resize-pane", "-Z", "-t", &window.window_id])?;
            }
            self.run(&["select-pane", "-t", &window.pane_id])?;
            self.run(&["resize-pane", "-Z", "-t", &window.pane_id])
        } else {
            self.run(&["attach-session", "-t", &window.session])
        }
    }

    fn output(&self, args: &[&str]) -> Result<String> {
        let output = Command::new(&self.config.command).args(args).output()?;
        if !output.status.success() {
            return Err(anyhow!(
                "tmux command failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn run(&self, args: &[&str]) -> Result<()> {
        let output = Command::new(&self.config.command).args(args).output()?;
        if output.status.success() {
            Ok(())
        } else {
            Err(anyhow!(
                "tmux command failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }
}

fn parse_window(line: &str) -> Result<Window> {
    let fields: Vec<&str> = line.split(SEP).collect();
    if fields.len() != 11 {
        return Err(anyhow!(
            "unexpected tmux inventory row with {} fields",
            fields.len()
        ));
    }
    Ok(Window {
        session: fields[0].into(),
        session_last_attached: fields[1].parse().unwrap_or_default(),
        window_id: fields[2].into(),
        window_name: fields[3].into(),
        pane_id: fields[4].into(),
        pane_pid: fields[5].parse().unwrap_or_default(),
        command: fields[6].into(),
        cwd: fields[7].into(),
        harness_id: (!fields[8].is_empty()).then(|| fields[8].into()),
        thread_id: (!fields[9].is_empty()).then(|| fields[9].into()),
        chat_title: (!fields[10].is_empty()).then(|| fields[10].into()),
    })
}

pub fn normalized_path(path: impl AsRef<Path>) -> PathBuf {
    fs::canonicalize(path.as_ref()).unwrap_or_else(|_| path.as_ref().to_path_buf())
}

fn unique_session_name(inventory: &[Window], cwd: &str) -> String {
    let base = Path::new(cwd)
        .file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "project".into());
    let existing: HashSet<&str> = inventory
        .iter()
        .map(|window| window.session.as_str())
        .collect();
    unique_name(&base, &existing)
}

fn unique_name(base: &str, existing: &HashSet<&str>) -> String {
    if !existing.contains(base) {
        return base.into();
    }
    (2..)
        .map(|suffix| format!("{base}-{suffix}"))
        .find(|candidate| !existing.contains(candidate.as_str()))
        .unwrap()
}

fn sanitize_name(value: &str) -> String {
    let mut result = String::new();
    let mut previous_dash = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() || character == '_' {
            result.push(character);
            previous_dash = false;
        } else if !previous_dash && !result.is_empty() {
            result.push('-');
            previous_dash = true;
        }
        if result.len() >= 24 {
            break;
        }
    }
    result.trim_matches('-').to_owned()
}

fn shell(value: &str) -> Cow<'_, str> {
    shell_escape::unix::escape(Cow::Borrowed(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::DEFAULT_HARNESS_ID;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn parses_structured_tmux_rows() {
        let line = "dotfiles\u{1f}100\u{1f}@2\u{1f}agents\u{1f}%2\u{1f}42\u{1f}codex\u{1f}/tmp/project\u{1f}codex\u{1f}thread-1\u{1f}Test chat";
        let window = parse_window(line).unwrap();
        assert_eq!(window.session, "dotfiles");
        assert_eq!(window.harness_id.as_deref(), Some(DEFAULT_HARNESS_ID));
        assert_eq!(window.thread_id.as_deref(), Some("thread-1"));
        assert_eq!(window.chat_title.as_deref(), Some("Test chat"));
    }

    #[test]
    fn makes_names_safe_and_unique() {
        assert_eq!(sanitize_name("Fix: API / Tests!"), "fix-api-tests");
        let existing = HashSet::from(["agent", "agent-2"]);
        assert_eq!(unique_name("agent", &existing), "agent-3");
    }

    #[test]
    fn ignores_unmanaged_shells_in_agent_windows() {
        let client = Client::new(TmuxConfig::default());
        let mut window = Window {
            session: "repo".into(),
            session_last_attached: 0,
            window_id: "@1".into(),
            window_name: "agents".into(),
            pane_id: "%1".into(),
            pane_pid: 1,
            command: "zsh".into(),
            cwd: "/repo".into(),
            harness_id: None,
            thread_id: None,
            chat_title: None,
        };
        assert!(!client.is_agent(&window));

        window.chat_title = Some("Managed chat".into());
        assert!(client.is_agent(&window));
    }

    #[test]
    fn shell_escapes_external_values() {
        assert_eq!(shell("a b; touch /tmp/no").as_ref(), "'a b; touch /tmp/no'");
    }

    #[test]
    fn manages_an_isolated_tmux_server() {
        if Command::new("tmux").arg("-V").output().is_err() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let socket = format!("cia-test-{}", std::process::id());
        let wrapper = directory.path().join("tmux-wrapper");
        let fake_codex = directory.path().join("fake codex");
        let fake_cia = directory.path().join("fake cia");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexport TMUX_TMPDIR={}\nexec tmux -L {socket} \"$@\"\n",
                shell(directory.path().to_string_lossy().as_ref())
            ),
        )
        .unwrap();
        std::fs::write(&fake_codex, "#!/bin/sh\nsleep 10\n").unwrap();
        std::fs::write(&fake_cia, "#!/bin/sh\nsleep 10 &\nwait\n").unwrap();
        for path in [&wrapper, &fake_codex, &fake_cia] {
            let mut permissions = std::fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(path, permissions).unwrap();
        }
        let output = Command::new(&wrapper)
            .args(["new-session", "-d", "-s", "project", "-c", "/tmp"])
            .output()
            .unwrap();
        if String::from_utf8_lossy(&output.stderr).contains("Operation not permitted") {
            return;
        }
        assert!(output.status.success());

        let client = Client::new(TmuxConfig {
            command: wrapper.to_string_lossy().into_owned(),
            ..TmuxConfig::default()
        });
        let inventory = client.inventory().unwrap();
        let window = client
            .open_agent(AgentLaunch {
                inventory: &inventory,
                cwd: "/tmp",
                title: "Test chat",
                harness_id: DEFAULT_HARNESS_ID,
                thread_id: Some("thread-1"),
                cia_command: fake_cia.to_str().unwrap(),
                agent_command: fake_codex.to_str().unwrap(),
                session_dir: None,
            })
            .unwrap();
        assert_eq!(window.thread_id.as_deref(), Some("thread-1"));
        assert_eq!(window.window_name, "agents");
        let mut child_commands = Vec::new();
        for _ in 0..20 {
            let processes = Command::new("ps")
                .args(["-ao", "ppid,args"])
                .output()
                .unwrap();
            child_commands = String::from_utf8_lossy(&processes.stdout)
                .lines()
                .filter_map(|line| {
                    let mut fields = line.split_whitespace();
                    (fields.next() == Some(window.pane_pid.to_string().as_str()))
                        .then(|| fields.collect::<Vec<_>>().join(" "))
                })
                .collect::<Vec<_>>();
            if child_commands
                .iter()
                .any(|command| command.contains("run-thread"))
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            child_commands
                .iter()
                .any(|command| command.contains("run-thread")),
            "unexpected pane child commands: {child_commands:?}"
        );
        assert!(child_commands
            .iter()
            .any(|command| command.contains("thread-1")));
        let second_inventory = client.inventory().unwrap();
        let second = client
            .open_agent(AgentLaunch {
                inventory: &second_inventory,
                cwd: "/tmp",
                title: "Other chat",
                harness_id: DEFAULT_HARNESS_ID,
                thread_id: Some("thread-2"),
                cia_command: fake_cia.to_str().unwrap(),
                agent_command: fake_codex.to_str().unwrap(),
                session_dir: None,
            })
            .unwrap();
        assert_eq!(second.window_id, window.window_id);
        assert_ne!(second.pane_id, window.pane_id);
        assert_eq!(second.chat_title.as_deref(), Some("Other chat"));
        let new_chat_inventory = client.inventory().unwrap();
        let new_chat = client
            .open_agent(AgentLaunch {
                inventory: &new_chat_inventory,
                cwd: "/tmp",
                title: "Named chat",
                harness_id: DEFAULT_HARNESS_ID,
                thread_id: None,
                cia_command: fake_cia.to_str().unwrap(),
                agent_command: fake_codex.to_str().unwrap(),
                session_dir: None,
            })
            .unwrap();
        assert_eq!(new_chat.window_id, window.window_id);
        assert_eq!(new_chat.thread_id, None);
        assert_eq!(new_chat.chat_title.as_deref(), Some("Named chat"));
        let _ = Command::new(&wrapper).arg("kill-server").status();
    }
}
