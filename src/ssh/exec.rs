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
    let mut channel = ssh.handle.channel_open_session().await?;
    channel.exec(true, command).await?;
    let mut exit_code = None;
    while let Some(msg) = channel.wait().await {
        match msg {
            ChannelMsg::ExitStatus { exit_status } => exit_code = Some(exit_status),
            ChannelMsg::Close => break,
            _ => {}
        }
    }
    Ok(exit_code)
}
