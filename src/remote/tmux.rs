use anyhow::{Result, bail};

use crate::ssh::{Ssh, exec};

pub fn session_name(id: &str) -> String {
    format!("agentssh-{id}")
}

/// The command exec'd on the PTY channel: -A attaches if the session exists,
/// creates it otherwise, so first connect and every reattach are identical.
pub fn attach_command(name: &str) -> String {
    format!("tmux new-session -A -s {name}")
}

pub async fn require_tmux(ssh: &Ssh, host: &str) -> Result<()> {
    match exec::probe(ssh, "command -v tmux >/dev/null 2>&1").await? {
        Some(0) => Ok(()),
        _ => bail!(
            "tmux is required on {host} for persistent shells and interactive sessions, \
             but was not found.\n\
             Install it there (e.g. apt install tmux / dnf install tmux), or use \
             `agentssh run` instead, which needs nothing on the remote."
        ),
    }
}

/// Does the remote tmux session still exist? Used after a clean channel exit to
/// tell "user detached (resumable)" apart from "shell exited (session over)".
pub async fn session_exists(ssh: &Ssh, name: &str) -> Result<bool> {
    Ok(matches!(
        exec::probe(ssh, &format!("tmux has-session -t {name} 2>/dev/null")).await?,
        Some(0)
    ))
}
