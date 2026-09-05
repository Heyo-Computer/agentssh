# agentssh skills for Claude Code

Four skills that let an agent do real work on remote hosts through `agentssh`,
so every command and its full output land in the local audit trail instead of
in an unrecorded `ssh` invocation.

| Skill | What it's for |
| --- | --- |
| `/ssh` | Run work on a host in a persistent shell: `/ssh to us2 and restart nginx` |
| `/ssh-contexts` | Add, inspect, and remove the named servers `/ssh` connects to |
| `/ssh-sessions` | Read back the audit trail — list, show, export, replay, reattach |
| `/ssh-files` | Move files over the audited channel (agentssh has no scp) |

## Install

```sh
cargo install --path .     # if agentssh isn't on PATH yet
./skills/install.sh        # symlinks the four skills into ~/.claude/skills
```

Symlinks, so editing a `SKILL.md` here takes effect in the next session.
`CLAUDE_SKILLS_DIR=.claude/skills ./skills/install.sh` installs into a project
instead; `./skills/install.sh --uninstall` removes them (it only removes links
that point back here).

## Use

```
/ssh to us2 and check whether nginx is healthy
/ssh-contexts add staging at 10.0.4.2 as deploy, over ssh-agent
/ssh-sessions show the last thing run on us2
/ssh-files put ./nginx.conf on us2 at /etc/nginx/nginx.conf
```

Claude also loads these on its own when a request implies remote work on a
configured context — the leading slash just makes it explicit.

## Layout

```
skills/
├── install.sh
├── ssh/SKILL.md
├── ssh-contexts/SKILL.md
├── ssh-sessions/SKILL.md
└── ssh-files/
    ├── SKILL.md
    └── scripts/{agentssh-put,agentssh-get}
```
