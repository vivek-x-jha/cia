# AGENTS.md

## Project

CIA is a Rust/Ratatui tmux dashboard for Codex and Pi chats. It reads agent history and switches/launches managed tmux panes; it should not mutate agent-owned history.

## Test and Build

Run commands through `zsh -lc` on this machine so dotfiles PATH/env are loaded.

For Rust changes, run the automated checks before building or installing:

```sh
zsh -lc 'cargo fmt --check'
zsh -lc 'cargo test'
zsh -lc 'cargo clippy --all-targets -- -D warnings'
```

For UI/input/backend behavior changes, manually evaluate from the current working tree when practical before installing:

```sh
zsh -lc 'cargo run'
```

After successful automated checks, update the installed binary for Rust or user-visible behavior changes unless the user explicitly asks not to. CIA is normally exercised through the installed `cia` executable, so this verifies the local release build and makes the change visible in normal use. For docs-only, prompt-only, or instruction-only changes, do not install unless needed.

```sh
zsh -lc 'cargo install --path .'
```

## Architecture

- `src/main.rs`: app wiring, events, commands
- `src/ui.rs`: Ratatui rendering/input
- `src/model.rs`: project/thread/live-pane reconciliation
- `src/agent.rs`: shared harness-neutral model
- `src/codex.rs`, `src/pi.rs`: history adapters
- `src/tmux.rs`: pane inventory, launch, switching, metadata
- `src/config.rs`, `src/state.rs`, `src/runner.rs`: config, CIA state, restore wrapper

## Rules

- Keep CIA history integrations read-only.
- Preserve keyboard-first UX; mouse support should not weaken keyboard flows.
- Keep tmux metadata and `cia run-thread` restore behavior stable.
- Prefer small harness-neutral abstractions over Codex-only branching.
- Update README when user-visible commands, keys, config, or behavior change.

## Checks by Change

- Rust changes: fmt, test, clippy.
- UI/input changes: manually run `cargo run` when practical.
- tmux changes: validate with an isolated tmux server when practical.
