use anyhow::Result;
use russh::ChannelMsg;
use tokio::io::AsyncWriteExt;

use crate::recording::asciicast::{EventKind, Writer};
use super::Ssh;

pub struct ExecOutcome {
    /// Remote exit code, or None if the connection dropped before one arrived.
    pub exit_code: Option<u32>,
}

/// Run one command over an exec channel, streaming remote stdout/stderr to the
/// local stdout/stderr unmodified while recording both as output events.
pub async fn run(ssh: &Ssh, command: &str, recorder: &mut Writer) -> Result<ExecOutcome> {
    let mut channel = ssh.handle.channel_open_session().await?;
    channel.exec(true, command).await?;

    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    let mut exit_code = None;

    while let Some(msg) = channel.wait().await {
        match msg {
            ChannelMsg::Data { ref data } => {
                stdout.write_all(data).await?;
                stdout.flush().await?;
                recorder.bytes(EventKind::Output, data)?;
            }
            ChannelMsg::ExtendedData { ref data, ext: 1 } => {
                stderr.write_all(data).await?;
                stderr.flush().await?;
                recorder.bytes(EventKind::Output, data)?;
            }
            ChannelMsg::ExitStatus { exit_status } => {
                exit_code = Some(exit_status);
            }
            ChannelMsg::Close => break,
            _ => {}
        }
    }
    Ok(ExecOutcome { exit_code })
}

/// Run a command and capture its exit status only (used for remote probes).
pub async fn probe(ssh: &Ssh, command: &str) -> Result<Option<u32>> {
    Ok(capture(ssh, command, None).await?.exit_code)
}

pub struct Captured {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<u32>,
}

impl Captured {
    pub fn ok(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// Trimmed stdout, for the small bookkeeping commands this is used for.
    pub fn out(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_string()
    }

    /// Trimmed stderr — what to put in an error message when a remote
    /// bookkeeping command fails.
    pub fn err(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }
}

/// Run one command over an exec channel and collect its output rather than
/// streaming it — for agentssh's own bookkeeping (tmux control commands,
/// reading a captured exit code), which is not part of the audited transcript.
///
/// `stdin` is written to the channel and then closed. That is how a command
/// line gets onto the remote host without being quoted into an argv: the bytes
/// travel as data, so no amount of quoting, backslashes, or newlines in the
/// user's command can change what the remote shell ends up running.
pub async fn capture(ssh: &Ssh, command: &str, stdin: Option<&[u8]>) -> Result<Captured> {
    let mut channel = ssh.handle.channel_open_session().await?;
    channel.exec(true, command).await?;
    if let Some(data) = stdin {
        channel.data(data).await?;
        channel.eof().await?;
    }
    let mut got = Captured { stdout: Vec::new(), stderr: Vec::new(), exit_code: None };
    while let Some(msg) = channel.wait().await {
        match msg {
            ChannelMsg::Data { ref data } => got.stdout.extend_from_slice(data),
            ChannelMsg::ExtendedData { ref data, .. } => got.stderr.extend_from_slice(data),
            ChannelMsg::ExitStatus { exit_status } => got.exit_code = Some(exit_status),
            ChannelMsg::Close => break,
            _ => {}
        }
    }
    Ok(got)
}
