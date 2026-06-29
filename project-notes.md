# Project Notes

## Future: provider usage limits

CIA should not show provider usage-limit time remaining yet.

Codex `/status` appears to use fresher live account/global rate-limit state than the rate-limit records persisted in Codex session JSONL. CIA currently reads agent history and session files read-only, so values derived from JSONL can drift from `/status`, especially after time passes or after `/status` is run without a new persisted `rate_limits` record.

Before adding usage-limit rows back to Status, investigate a source of truth that matches the harness CLI:

- Codex app-server/status or remote-control API, if available.
- A read-only Codex state/cache file that is updated by `/status`.
- Pi equivalent, if Pi exposes model/provider usage or context data.

Until then, avoid showing 5h/weekly/usage time remaining for any provider.
