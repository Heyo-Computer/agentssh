# agentssh

Audited SSH sessions for AI agents. Remote servers are stored as named
**contexts**; an agent opens sessions by context name and never touches
credentials. Every session — commands and full terminal output — is recorded
locally and can be replayed in the terminal or a web UI.

An agent's normal mode is a **persistent shell**: a real login shell kept alive
on the host inside `tmux`, which successive commands are typed into, so working
directory, environment, and shell state carry across calls and the whole run of
work replays as one terminal session. Interactive sessions use the same
mechanism, so a dropped connection reconnects with processes, working
directory, and environment intact.

## Quick start

```sh
# Store a server as a context (config lands in ~/.config/agentssh/contexts.toml, mode 0600)
agentssh context add prod --host 10.0.4.2 --user deploy --key ~/.ssh/id_ed25519
# ...or delegate auth to your running ssh-agent (no secret stored at all)
agentssh context add prod --host 10.0.4.2 --user deploy --agent

# Persistent shell (the primary agent mode): state carries between calls
agentssh exec prod -- cd /srv/app
agentssh exec prod -- export RAILS_ENV=production
agentssh exec prod -- ./bin/status                  # runs in /srv/app, env set
agentssh exec prod -- eval 'ls | wc -l'             # shell syntax, same shell
agentssh exec prod --timeout 30 -- ./deploy.sh      # Ctrl-C on timeout

agentssh shell list                                 # open shells
agentssh shell stop prod                            # end one, kill remote tmux

# One-shot: own connection, no shell state, exact stdout/stderr
agentssh run prod -- systemctl status nginx

# Interactive shell inside remote tmux; auto-reconnects on drops
agentssh connect prod
# detach with tmux prefix (C-b d), resume later — this also attaches to the
# persistent shell an agent is working in, live:
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
- **Persistent shells** run `tmux new-session -d` with a POSIX login shell.
  Each `agentssh exec` writes the command to the remote as channel *data* (so
  no quoting layer can alter it), types one fixed wrapper line into the shell
  that sources it with output redirected to a file, and blocks on a second
  channel until the exit-code file appears. Commands therefore run in the live
  shell — `cd` and `export` stick — while their output is captured exactly,
  away from the pane's prompts and echo. Output is not streamed and stderr is
  merged into stdout; use `run` when you need either.
- **Recordings** are [asciicast v2](https://docs.asciinema.org/manual/asciicast/v2/)
  files in `~/.local/share/agentssh/recordings/` (0600), one per logical
  session across all reconnects — and for a shell session, across every command
  run in it — playable with `asciinema play` or the built-in web player.
  Session metadata (status, exit codes, connection segments, and the per-command
  log of a shell session) is in a local SQLite database. Output from channels
  with no PTY behind them is recorded with CRLF line endings, because a
  terminal treats a bare LF as "down one row, same column" and would otherwise
  replay it as a staircase.
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
  half-executed command can't be safely retried. It is the right mode for
  byte-exact stdout (file transfer), for streaming output, and for hosts
  without `tmux`.

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
- A command that times out is sent Ctrl-C and its partial output is kept. If it
  ignores SIGINT the shell is left busy and the next `exec` refuses rather than
  typing over it; `agentssh shell interrupt` and `agentssh shell stop` are the
  ways out. A result that arrives after agentssh stopped waiting is folded into
  the audit trail on the next call.
- The web UI has no authentication and binds 127.0.0.1; binding anything else
  requires `--allow-remote`.
- `tmux` must be installed on the remote host for `exec`/`connect`/`attach`
  (`run` needs nothing).
