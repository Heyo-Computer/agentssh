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
agentssh exec <context> -- <argv...>
```

`exec` runs your command in a **persistent shell** on the host — a real login
shell kept alive inside remote `tmux`. It opens on your first `exec` and every
later `exec` types into the same shell, so working directory, exported
variables, an activated virtualenv, and shell history all carry across calls:

```bash
agentssh exec us2 -- cd /srv/app        # sticks
agentssh exec us2 -- export RAILS_ENV=production
agentssh exec us2 -- ./bin/status       # runs in /srv/app, with RAILS_ENV set
```

Exit codes pass through: the remote command's code becomes the local one.
`255` means transport failure or timeout, not a remote exit code.

## The one rule that trips everything up

Everything after `--` is argv. agentssh shell-escapes each argument and joins
them into a single command line, so **shell syntax written as one argument is
not shell syntax** — it becomes the command name:

```bash
agentssh exec us2 -- 'echo hi | wc -c'        # ✗ "command not found", exit 127
agentssh exec us2 -- eval 'echo hi | wc -c'   # ✓ prints 6
```

Reach for `eval '...'` whenever you need a pipe, redirect, glob, `&&`/`||`, a
remote `$VAR`, or a heredoc. `eval` is a shell builtin, so the script runs
**inside** the persistent shell and its `cd`s and `export`s stick:

```bash
agentssh exec us2 -- eval 'cd /srv/app && export TAG=$(git rev-parse --short HEAD)'
agentssh exec us2 -- eval 'echo "deploying $TAG from $PWD"'   # both still set
```

`sh -c '...'` also works and is what you want for a **throwaway** subshell —
but it is a separate process, so nothing it changes survives the call:

```bash
agentssh exec us2 -- sh -c 'cd /tmp && pwd'   # prints /tmp
agentssh exec us2 -- pwd                      # unchanged — still /srv/app
```

Plain argv (`uname -sr`, `systemctl status nginx`) needs no wrapper at all, and
multi-word arguments keep their quoting: `-- grep 'foo bar' file` is correct.

### Batch your probes

Each `agentssh exec` opens a **fresh SSH connection** to type one line into the
shell. Three diagnostics should be one call, not three:

```bash
agentssh exec us2 -- eval 'uptime; df -h /; free -m; systemctl --failed --no-pager'
```

### Always bound anything that could hang

```bash
agentssh exec us2 --timeout 60 -- ./deploy.sh
```

On timeout agentssh sends the shell a **Ctrl-C**, exits `255`, and keeps
whatever the command printed before it was cut off. If the command ignores
SIGINT the shell is left busy, and the next `exec` refuses rather than typing
over a running command — that message tells you what to do:

```bash
agentssh shell interrupt us2     # try again, harder
agentssh shell stop us2          # give up and kill the shell
```

A command whose result arrives after you stopped waiting is not lost: the next
`exec` collects its output and exit code into the audit trail first.

## When to use `run` instead

```bash
agentssh run <context> -- <argv...>
```

`run` is the one-shot form: its own connection, its own exec channel, no shell
state, no tmux. Use it when you need one of the three things `exec` gives up:

- **Byte-exact stdout.** `exec` merges stderr into stdout (that is what the
  recording shows). If you're capturing output locally — `$(...)`, `> file`,
  piping base64 — use `run`.
- **Streaming.** `exec` returns a command's output when it finishes; `run`
  streams it as it arrives.
- **A host without `tmux`.** `exec` needs it; `run` needs nothing.

`run` is also the right call for anything that must not share state with the
rest of your work, and it is what `/ssh-files` uses under the hood.

## What this channel cannot do

- **No stdin.** Nothing local can be piped in, and no remote prompt can be
  answered. Use non-interactive flags: `-y`, `--no-pager`,
  `DEBIAN_FRONTEND=noninteractive`. `sudo` works only if it's NOPASSWD.
- **No TTY.** Commands run with their output redirected to a file, so `top`,
  `vim`, `less`, and anything curses-based will misbehave. Use `top -bn1`,
  `journalctl --no-pager`, `cat`.
- **No file transfer built in.** Use `/ssh-files` — it moves files over this
  same audited channel.
- **Nothing is resumable mid-command.** If the connection drops while a command
  is running, the command keeps going in the remote shell and its result is
  collected on your next `exec` — but agentssh will not re-run it for you.

## Long-running work

For something that must outlive the command, detach it on the remote:

```bash
agentssh exec us2 -- eval 'nohup ./long-job.sh > /tmp/job.log 2>&1 & echo started'
agentssh exec us2 -- tail -n 50 /tmp/job.log      # poll later
```

## Shell housekeeping

```bash
agentssh shell list        # open shells, how many commands each has run
agentssh shell stop us2    # end one and kill its remote tmux session
agentssh exec us2 --fresh -- pwd   # discard the old shell, start clean
```

Stop the shell when you're finished with the host — it costs nothing to
reopen, and leaving `tmux` sessions behind on servers is untidy. A shell whose
remote `tmux` has died (host rebooted, someone typed `exit`) is detected and
replaced automatically, so you never have to check first.

## When you need a real interactive shell

You can't drive one. Hand it to the user — tell them to run, in this session:

```
! agentssh connect <context>
```

They can also **attach to the shell you are working in**, which is often more
useful — same tmux session, live:

```
! agentssh attach <shell-id-prefix>      # id from: agentssh shell list
```

Detach with `C-b d`. Note that interactive sessions record keystrokes too;
mention `--no-record-input` if they'll be typing a password.

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
