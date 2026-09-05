---
name: ssh-sessions
description: Read back the agentssh audit trail — list recorded remote sessions, show one session's metadata and output, export or replay its terminal recording, reattach a detached interactive session, or serve the playback web UI. Use when the user asks what was run on a server, wants to see or replay a past session, asks for proof or an audit trail of remote work, or wants to resume a dropped interactive session.
argument-hint: [list | show <id> | export <id> | replay <id> | attach <id> | web]
allowed-tools: [Bash, Read, Glob, Grep]
---

# /ssh-sessions — the agentssh audit trail

Every `agentssh run` and `agentssh connect` writes an
[asciicast v2](https://docs.asciinema.org/manual/asciicast/v2/) recording to
`~/.local/share/agentssh/recordings/` (mode `0600`), with metadata in a local
SQLite database. One recording covers one logical session, spanning all its
reconnects.

## List

```bash
agentssh sessions list                      # 25 most recent
agentssh sessions list --context us2        # one host
agentssh sessions list --active             # only active or resumable
agentssh sessions list --limit 100
```

Columns: id, context, kind (`run` / `connect`), status, start time, exit code,
command. A blank exit code means no remote exit status arrived — the connection
dropped, or the session timed out.

## Show one session

```bash
agentssh sessions show <id>
```

Takes a **unique id prefix**, not just the full id — `agentssh sessions show
1967064d` is enough. Prints metadata, exit code, the recording path, the
connection segments (one row per connect/reconnect, so drops are visible), and
a tail of the output.

For the *full* output rather than the tail, export it:

```bash
agentssh sessions export <id> --format txt
```

## Export and replay

```bash
agentssh sessions export <id> --format txt                    # plain text, to stdout
agentssh sessions export <id> --format asciicast -o s.cast    # the raw recording
asciinema play s.cast                                         # replay in the terminal
```

`--format txt` is what you want when you need to *read* or grep a session —
pipe it to `grep`, or write it to a file and Read that. Prefer it over opening
the `.cast` file directly; the raw asciicast is a JSON event stream, one line
per output chunk, and reading it costs many times more tokens than the text.

For a session that recorded keystrokes, note that `"i"` events in the
asciicast are typed input, including anything typed at a remote prompt.

## Web playback UI

```bash
agentssh web                    # http://127.0.0.1:8787
```

Binds loopback and has **no authentication** — binding anything else requires
`--allow-remote`, which you should not pass unless the user explicitly asks and
understands it exposes an unauthenticated UI. Run it in the background and hand
the user the URL.

## Reattach a dropped or detached session

Interactive sessions run inside a remote `tmux`, so their processes survive a
disconnect. Find a resumable one with `agentssh sessions list --active`, then
have the **user** run it — it needs a terminal you can't drive:

```
! agentssh attach <id-prefix>
```

## Delete

```bash
agentssh sessions rm <id>
```

This destroys the recording — it is the audit trail. Confirm with the user
first, and never delete sessions in bulk to "clean up" unless they ask for
exactly that.
