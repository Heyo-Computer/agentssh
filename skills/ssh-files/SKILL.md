---
name: ssh-files
description: Copy files to and from a remote host over the audited agentssh channel — upload a config, script, or binary to a server, or pull down a log, config, or artifact. Use whenever a file needs to move between this machine and an agentssh context, since agentssh has no scp/sftp of its own and plain scp would bypass the audit trail.
argument-hint: put|get <context> <src> <dest>
allowed-tools: [Bash, Read, Write, Glob, Grep]
---

# /ssh-files — move files over the audited channel

`agentssh` has no scp or sftp, and `agentssh run` forwards no stdin — so a file
can't be piped to a remote command. Two scripts ship with this skill and handle
it properly, sending the payload as base64 inside the command line and
verifying the result by sha256.

They use `agentssh run`, not `agentssh exec`, on purpose: a transfer needs
byte-exact stdout with stderr kept separate, which is exactly what the
one-shot channel gives and what a persistent shell does not.

Use these rather than reaching for `scp`/`rsync` directly: those bypass the
audit trail and need credentials this session doesn't hold.

## Upload

```bash
~/.claude/skills/ssh-files/scripts/agentssh-put <context> <local-file> <remote-path> [--mode 0644]
```

Creates the remote parent directory, transfers, verifies the checksum, and
exits non-zero on any mismatch. `--mode` chmods the result (use it for scripts:
`--mode 0755`). Progress and the verified sha256 go to stderr.

## Download

```bash
~/.claude/skills/ssh-files/scripts/agentssh-get <context> <remote-path> <local-file>
```

Verifies before writing — on a checksum mismatch nothing lands locally.

If the file is something you want to *read* rather than keep, skip the
round trip and just print it:

```bash
agentssh exec <context> -- cat /etc/nginx/nginx.conf
agentssh exec <context> -- tail -n 200 /var/log/syslog
```

## Size limits, and why chunking exists

Linux caps a single exec argument at 128 KiB (`MAX_ARG_STRLEN`), and base64
inflates by 4/3 — so an upload larger than ~96 KiB will not fit in one command
line. `agentssh-put` splits the payload into 64 KiB chunks and reassembles them
remotely, so this is handled for you; it's why a large upload shows as several
`agentssh run` calls in the audit trail.

Each chunk is a full SSH connection, so cost scales with size: a few hundred KB
is fine, tens of MB is slow and noisy in the session log. For anything that
large, prefer having the remote fetch it itself:

```bash
agentssh exec <context> -- eval 'curl -fsSL <url> -o /tmp/artifact && sha256sum /tmp/artifact'
```

Requirements: `base64` and `sha256sum` (or `shasum`) on the remote — present on
any normal Linux or macOS host.

## Editing a remote file

Don't try to drive `sed -i` blind. Pull it down, edit it locally with real
tools, push it back — and back up the original first:

```bash
agentssh exec us2 -- cp /etc/nginx/nginx.conf /etc/nginx/nginx.conf.bak
~/.claude/skills/ssh-files/scripts/agentssh-get us2 /etc/nginx/nginx.conf ./nginx.conf
# edit ./nginx.conf locally
~/.claude/skills/ssh-files/scripts/agentssh-put us2 ./nginx.conf /etc/nginx/nginx.conf
agentssh exec us2 -- nginx -t          # validate before reloading
```

Confirm with the user before overwriting any file outside `/tmp` on a remote
host, and always before the reload/restart that follows.

## Secrets

Anything you upload is recorded in the session log as base64 — readable by
anyone who can replay it. Don't push keys, tokens, or credential files through
this channel; tell the user to place those on the host themselves.
