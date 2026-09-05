# agentssh

Audited SSH sessions for AI agents. Remote servers are stored as named
**contexts**; an agent opens sessions by context name and never touches
credentials. Every session — commands and full terminal output — is recorded
locally and can be replayed in the terminal or a web UI. Interactive sessions
run inside a remote `tmux` session, so a dropped connection reconnects
automatically with running processes, working directory, and environment
intact.

## Quick start

```sh
# Store a server as a context (config lands in ~/.config/agentssh/contexts.toml, mode 0600)
agentssh context add prod --host 10.0.4.2 --user deploy --key ~/.ssh/id_ed25519
# ...or delegate auth to your running ssh-agent (no secret stored at all)
agentssh context add prod --host 10.0.4.2 --user deploy --agent

# One-shot command (the primary agent mode): streams output, exits with the remote exit code
agentssh run prod -- systemctl status nginx
agentssh run prod --timeout 30 -- ./deploy.sh

# Interactive shell inside remote tmux; auto-reconnects on drops
agentssh connect prod
# detach with tmux prefix (C-b d), resume later:
agentssh attach <session-id-prefix>

# Audit trail
agentssh sessions list
agentssh sessions show <id>            # metadata, connection segments, output tail
agentssh sessions export <id> --format txt
agentssh sessions export <id> --format asciicast -o session.cast

# Playback web UI (localhost only by default)
agentssh web                           # http://127.0.0.1:8787
```

## How it works

- **Contexts** live in `~/.config/agentssh/contexts.toml` (0600, loader refuses
  looser permissions). Auth is a key file path or ssh-agent; key passphrases
  are prompted, never stored, and never passed via argv or env.
- **Recordings** are [asciicast v2](https://docs.asciinema.org/manual/asciicast/v2/)
  files in `~/.local/share/agentssh/recordings/` (0600), one per logical
  session across all reconnects, playable with `asciinema play` or the built-in
  web player. Session metadata (status, exit codes, connection segments) is in
  a local SQLite database.
- **Reconnect**: interactive sessions exec `tmux new-session -A` on the remote,
  so the shell survives transport drops. On a drop, agentssh retries with
  exponential backoff (0.5s → 15s, 10 attempts), then marks the session
  detached for a later `attach`. Reconnects are visible as marker events in the
  recording and as segment rows in `sessions show`.
- **Host keys** are checked against `~/.ssh/known_hosts`. The default
  `accept-new` policy learns unknown hosts (printing the fingerprint) but a
  *changed* key always fails, under every policy. Use
  `--host-key-policy strict` to also refuse unknown hosts.
- **`run` mode** uses a plain exec channel (no tmux) and stores the exact
  command line for the audit trail. A dropped `run` is not resumed — a
  half-executed command can't be safely retried.

## Claude Code skills

`skills/` holds four Claude Code skills that drive this tool, so an agent's
remote work lands in the audit trail instead of an unrecorded `ssh` call:
`/ssh` (run work on a host), `/ssh-contexts`, `/ssh-sessions`, and
`/ssh-files` (transfer, since agentssh has no scp). Install with
`./skills/install.sh` — it symlinks them into `~/.claude/skills`. See
[skills/README.md](skills/README.md).

## Notes

- Interactive recordings include keystrokes (`"i"` events) for full
  auditability; pass `--no-record-input` if secrets will be typed into the
  remote (e.g. sudo passwords). Remote *output* is always recorded.
- The web UI has no authentication and binds 127.0.0.1; binding anything else
  requires `--allow-remote`.
- `tmux` must be installed on the remote host for `connect`/`attach`
  (`run` needs nothing).
