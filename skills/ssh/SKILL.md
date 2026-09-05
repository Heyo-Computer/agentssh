---
name: ssh
description: Run work on a remote server through agentssh — an audited SSH channel where servers are named contexts and every command plus its full output is recorded locally. Use for "/ssh to us2 and restart nginx", or any time the user asks to check, inspect, deploy, restart, tail logs, or otherwise do something on a named remote host or server. Also use when a task needs a remote shell and the host is one of the configured agentssh contexts.
argument-hint: to <context> and <what to do>
allowed-tools: [Bash, Read, Write, Edit, Glob, Grep]
---

# /ssh — do work on a remote host, audited

`agentssh` opens SSH sessions by **context name**. You never see or handle
credentials; auth is an ssh-agent key or a key file the context points at.
Every command and every byte of remote output is recorded to a local
asciicast, so the whole session is replayable afterwards (see `/ssh-sessions`).

## Step 1 — resolve the context

Parse the target host out of `$ARGUMENTS` (the token after "to", "on", or "@",
e.g. `/ssh to us2 and check disk` → `us2`), then confirm it exists:

```bash
agentssh context list
```

If the name isn't in that list, **stop and show the user the list** — do not
guess at a near-match, and do not fall back to plain `ssh`. If no context was
named at all and exactly one exists, use it; otherwise ask which one.

## Step 2 — run the work

```bash
agentssh run <context> -- <argv...>
```

Exit codes pass through: the remote command's code becomes the local one.
`255` means transport failure or timeout, not a remote exit code.
Remote stdout/stderr stream through unmodified, so `> file`, `| grep`, and
`$(...)` capture on the **local** side all work normally.

### The one rule that trips everything up

Everything after `--` is argv. agentssh shell-escapes each argument and joins
them into a single remote command line, so **shell syntax written as one
argument is not shell syntax** — it becomes the command name:

```bash
agentssh run us2 -- 'echo hi | wc -c'      # ✗ "command not found", exit 127
agentssh run us2 -- sh -c 'echo hi | wc -c' # ✓ prints 6
```

Wrap in `sh -c '...'` whenever you need a pipe, redirect, glob, `&&`/`||`,
a remote `$VAR`, a `cd`, or a heredoc. Plain argv (`uname -sr`,
`systemctl status nginx`) needs no wrapper.

### Batch your probes

Each `agentssh run` opens a **fresh SSH connection**. Three diagnostics should
be one call, not three:

```bash
agentssh run us2 -- sh -c 'uptime; df -h /; free -m; systemctl --failed --no-pager'
```

### Always bound anything that could hang

```bash
agentssh run us2 --timeout 60 -- ./deploy.sh
```

A timed-out session is killed and exits `255`, with the partial output still
recorded.

## What this channel cannot do

- **No stdin.** Nothing local can be piped in, and no remote prompt can be
  answered. Use non-interactive flags: `-y`, `--no-pager`,
  `DEBIAN_FRONTEND=noninteractive`, `ssh-keyscan` over `ssh` etc. `sudo` works
  only if it's NOPASSWD for that user.
- **No TTY.** `top`, `vim`, `less`, and anything curses-based will misbehave.
  Use `top -bn1`, `journalctl --no-pager`, `cat`.
- **No file transfer built in.** Use `/ssh-files` — it moves files over this
  same audited channel.
- **`run` is not resumable.** If the connection drops mid-command, agentssh
  deliberately does not retry: a half-executed command can't be safely
  re-run. Re-run it yourself only once you know it's safe to.
- **Each `run` starts in the login directory.** Use `sh -c 'cd /srv/app && ...'`.

## Long-running work

`run` holds the connection for the command's lifetime. For something that must
outlive the session, detach it on the remote:

```bash
agentssh run us2 -- sh -c 'nohup ./long-job.sh > /tmp/job.log 2>&1 & echo started'
agentssh run us2 -- tail -n 50 /tmp/job.log      # poll later
```

## When you need a real interactive shell

You can't drive one. Hand it to the user — tell them to run, in this session:

```
! agentssh connect <context>
```

That opens a shell inside a remote `tmux` session (so a dropped link
reconnects with processes and cwd intact), still fully recorded. Detach with
`C-b d`; resume later with `agentssh attach <session-id-prefix>`. Note that
interactive sessions record keystrokes too — mention `--no-record-input` if
they'll be typing a password.

## Before destructive commands

Confirm with the user first, quoting the exact command line you're about to
run, for anything that: deletes data (`rm -rf`, `DROP`, `truncate`), restarts
or stops a service, reboots, reconfigures a firewall or network, removes
packages, writes to a block device, or rotates credentials. Treat any context
whose name or host contains `prod`/`production` as needing that confirmation
for *any* state change, not just the list above.

Read-only inspection (`status`, `df`, `journalctl`, `cat`, `ls`, `ps`) needs no
confirmation — just run it.

## Report back

Say which context you touched and what the remote exit code was. If the user
may want the replay, the session id is the top row of `agentssh sessions list`
and `/ssh-sessions` covers reading it back.
