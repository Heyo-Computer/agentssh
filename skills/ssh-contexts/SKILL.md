---
name: ssh-contexts
description: Add, inspect, or remove agentssh server contexts — the named remote hosts that /ssh connects to. Use when the user wants to register a new server ("add my staging box as staging"), see which servers are configured, change how a host authenticates or how strictly its host key is checked, or remove a server. Also use when /ssh reports an unknown context name.
argument-hint: [add <name> | list | show <name> | remove <name>]
allowed-tools: [Bash, Read, Glob, Grep]
---

# /ssh-contexts — manage agentssh server contexts

A **context** is a named remote host: address, user, and how to authenticate.
`/ssh` and `/ssh-files` only ever refer to hosts by context name, so the
credential never reaches the command line, the environment, or a transcript.

Config lives in `~/.config/agentssh/contexts.toml`, mode `0600`. **The loader
refuses to read it if the permissions are looser than that** — if a command
errors about permissions, `chmod 600 ~/.config/agentssh/contexts.toml`.

## List / inspect

```bash
agentssh context list            # name, host:port, user, auth method, host-key policy
agentssh context show <name>     # same for one context; prints a key *path*, never key contents
```

Neither ever prints a secret, so both are safe to run and safe to show the
user verbatim.

## Add

Prefer ssh-agent — it stores no secret at all, not even a path:

```bash
agentssh context add <name> --host <addr> --user <user> --agent
```

Check the agent actually has a key first (`ssh-add -l`); if it's empty, the
connection will fail at auth time with nothing useful to fall back on.

With a key file instead:

```bash
agentssh context add <name> --host <addr> --user <user> --key ~/.ssh/id_ed25519
```

The key path is stored; the key is never copied, and a passphrase is prompted
at connect time and never persisted. `--key` and `--agent` are mutually
exclusive. Add `--port <n>` for a non-22 port.

`add` **replaces** an existing context of the same name without warning — check
`agentssh context list` first and confirm with the user before reusing a name.

### Host-key policy

```bash
--host-key-policy accept-new   # default: learn unknown hosts, print the fingerprint
--host-key-policy strict       # refuse unknown hosts too
```

Under **both** policies a *changed* host key is a hard failure — that's the
MITM case and agentssh never silently accepts it. Keys are checked against
`~/.ssh/known_hosts`. Use `strict` for anything production-facing; if the user
wants strict, the host must already be in `known_hosts`.

If a connection fails on a changed host key, do **not** edit `known_hosts` to
make it pass. Report it to the user and let them decide — a legitimately
rebuilt host is their call to make, not yours.

## Remove

```bash
agentssh context remove <name>
```

Confirm with the user first. Recorded sessions for that context survive removal
and stay readable via `/ssh-sessions`.

## Verify a new context works

One cheap read-only round trip, right after adding:

```bash
agentssh run <name> -- uname -sr
```

Exit `0` means address, auth, and host key all check out. Exit `255` is a
transport or auth failure — re-check with `agentssh context show <name>` and
`ssh-add -l`.

Use `run` for this check, not `exec`: it needs nothing on the remote, so it
isolates a connectivity problem from a missing `tmux`. If you then want the
persistent shell `/ssh` normally works in, confirm `tmux` is there too:

```bash
agentssh exec <name> -- uname -sr
```

If that fails with "tmux is required", install it on the host or stick to
`agentssh run` for that context.
