use std::io::Read as _;

use anyhow::{Context as _, Result};
use russh::ChannelMsg;
use tokio::io::AsyncWriteExt;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use crate::recording::asciicast::{EventKind, Writer};
use crate::remote::tmux;
use crate::{config, ssh, store};
use super::reconnect;

/// `agentssh connect <context>`: new interactive session inside remote tmux.
pub async fn connect(context_name: &str, record_input: bool) -> Result<i32> {
    let ctx = config::get_context(context_name)?;
    let conn = store::open()?;

    let id = store::new_session_id();
    let recording = store::recording_path(&id)?;
    let (cols, rows) = term_size();
    let session = store::Session {
        id: id.clone(),
        context: context_name.to_string(),
        kind: "connect".into(),
        command: None,
        tmux_name: Some(tmux::session_name(&id)),
        cols: Some(cols as u32),
        rows: Some(rows as u32),
        started_at: store::now_rfc3339(),
        ended_at: None,
        exit_code: None,
        status: "active".into(),
        recording: recording.to_string_lossy().into_owned(),
    };
    let recorder = Writer::create(
        &recording,
        cols as u32,
        rows as u32,
        &format!("{context_name} (connect)"),
    )?;
    store::create_session(&conn, &session)?;
    eprintln!("agentssh: session {id} (resume later with: agentssh attach {})", &id[..6]);
    drive(&conn, &ctx, &session, record_input, recorder, true).await
}

/// `agentssh attach <id>`: resume a detached/dropped session.
pub async fn attach(prefix: &str, record_input: bool) -> Result<i32> {
    let conn = store::open()?;
    let session = store::find_session(&conn, prefix)?;
    // Anything with a remote tmux session behind it can be attached — that
    // includes an agent's persistent `shell`, so a user can watch or take over
    // the session an agent is working in.
    if session.tmux_name.is_none() {
        anyhow::bail!(
            "session {} was a one-shot `run`; only tmux-backed sessions (connect, shell) can be attached",
            session.id
        );
    }
    if session.status == "closed" {
        anyhow::bail!(
            "session {} is closed (its remote tmux session ended); start a new one with: agentssh connect {}",
            session.id, session.context
        );
    }
    let ctx = config::get_context(&session.context)?;
    let recorder = Writer::append(std::path::Path::new(&session.recording), &session.started_at)?;
    store::set_status(&conn, &session.id, "active")?;
    eprintln!("agentssh: reattaching session {}", session.id);
    drive(&conn, &ctx, &session, record_input, recorder, false).await
}

/// Local terminal size with a sane floor: a degenerate 0x0 (no real tty)
/// would otherwise propagate to the remote PTY and the recording header.
pub fn term_size() -> (u16, u16) {
    match crossterm::terminal::size() {
        Ok((c, r)) if c > 0 && r > 0 => (c, r),
        _ => (80, 24),
    }
}

enum PumpEnd {
    /// Channel closed normally; carries the remote exit status if one arrived.
    Clean(Option<u32>),
    /// Transport died underneath us.
    Dropped,
}

/// Supervisor: connect, pump, and reconnect with backoff until the session
/// ends cleanly or the user gives up. One recording spans all segments.
async fn drive(
    conn: &rusqlite::Connection,
    ctx: &config::SshContext,
    session: &store::Session,
    record_input: bool,
    mut recorder: Writer,
    mut first_segment: bool,
) -> Result<i32> {
    let tmux_name = session.tmux_name.clone().unwrap();
    let mut stdin_rx = spawn_stdin_reader();
    let mut winch = signal(SignalKind::window_change())?;
    let mut segment_no: u32 = 0;

    loop {
        // (Re)establish the transport, with backoff after the first failure.
        let ssh = if first_segment {
            first_segment = false;
            match ssh::connect(ctx).await {
                Ok(s) => s,
                Err(e) => {
                    store::finish_session(conn, &session.id, "closed", None)?;
                    return Err(e);
                }
            }
        } else {
            match reconnect_with_backoff(ctx, &session.id).await {
                Some(s) => s,
                None => {
                    store::set_status(conn, &session.id, "detached")?;
                    eprintln!(
                        "agentssh: giving up after {} attempts; resume later with: agentssh attach {}",
                        reconnect::MAX_ATTEMPTS,
                        &session.id[..6]
                    );
                    return Ok(255);
                }
            }
        };

        if let Err(e) = tmux::require_tmux(&ssh, &ctx.host).await {
            store::finish_session(conn, &session.id, "closed", None)?;
            return Err(e);
        }

        segment_no += 1;
        let seg = store::start_segment(conn, &session.id)?;
        if segment_no > 1 {
            recorder.event(EventKind::Marker, &format!("reconnected (segment {segment_no})"))?;
        }

        let end = pump(&ssh, &tmux_name, &mut recorder, &mut stdin_rx, &mut winch, record_input).await;

        match end {
            Ok(PumpEnd::Clean(code)) => {
                // tmux client exited: detach and shell-exit look the same, so ask
                // the server whether the tmux session survived.
                let survived = tmux::session_exists(&ssh, &tmux_name).await.unwrap_or(false);
                store::end_segment(conn, seg, if survived { "detached" } else { "closed" })?;
                if survived {
                    store::set_status(conn, &session.id, "detached")?;
                    eprintln!(
                        "agentssh: detached from session {}; resume with: agentssh attach {}",
                        session.id,
                        &session.id[..6]
                    );
                } else {
                    store::finish_session(conn, &session.id, "closed", code.map(|c| c as i64))?;
                    eprintln!("agentssh: session {} closed", session.id);
                }
                return Ok(code.unwrap_or(0) as i32);
            }
            Ok(PumpEnd::Dropped) | Err(_) => {
                store::end_segment(conn, seg, "dropped")?;
                recorder.event(EventKind::Marker, "connection dropped")?;
                eprintln!("\r\nagentssh: connection to {} lost; reconnecting...", ctx.host);
                // loop: reconnect_with_backoff takes it from here
            }
        }
    }
}

async fn reconnect_with_backoff(ctx: &config::SshContext, _session_id: &str) -> Option<ssh::Ssh> {
    for attempt in 1..=reconnect::MAX_ATTEMPTS {
        let delay = reconnect::delay(attempt);
        tokio::time::sleep(delay).await;
        eprintln!("agentssh: reconnecting to {} (attempt {attempt}/{})", ctx.host, reconnect::MAX_ATTEMPTS);
        match ssh::connect(ctx).await {
            Ok(s) => return Some(s),
            Err(e) => eprintln!("agentssh: reconnect failed: {e:#}"),
        }
    }
    None
}

/// One connected segment: raw-mode PTY passthrough with recording.
async fn pump(
    ssh: &ssh::Ssh,
    tmux_name: &str,
    recorder: &mut Writer,
    stdin_rx: &mut mpsc::Receiver<Vec<u8>>,
    winch: &mut tokio::signal::unix::Signal,
    record_input: bool,
) -> Result<PumpEnd> {
    let channel = ssh.handle.channel_open_session().await?;
    let (cols, rows) = term_size();
    channel
        .request_pty(
            false,
            &std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into()),
            cols as u32,
            rows as u32,
            0,
            0,
            &[],
        )
        .await?;
    channel.exec(true, tmux::attach_command(tmux_name)).await?;
    recorder.resize(cols, rows)?;

    let _raw = RawModeGuard::enable()?;
    let mut channel = channel;
    let mut stdout = tokio::io::stdout();
    let mut exit_status = None;

    loop {
        tokio::select! {
            biased;
            input = stdin_rx.recv() => {
                match input {
                    Some(bytes) => {
                        channel.data(&bytes[..]).await.map_err(anyhow::Error::from)?;
                        if record_input {
                            recorder.bytes(EventKind::Input, &bytes)?;
                        }
                    }
                    None => {
                        channel.eof().await.ok();
                    }
                }
            }
            _ = winch.recv() => {
                let (c, r) = term_size();
                channel.window_change(c as u32, r as u32, 0, 0).await.ok();
                recorder.resize(c, r)?;
            }
            msg = channel.wait() => {
                match msg {
                    Some(ChannelMsg::Data { ref data }) => {
                        stdout.write_all(data).await?;
                        stdout.flush().await?;
                        recorder.bytes(EventKind::Output, data)?;
                    }
                    Some(ChannelMsg::ExtendedData { ref data, .. }) => {
                        stdout.write_all(data).await?;
                        stdout.flush().await?;
                        recorder.bytes(EventKind::Output, data)?;
                    }
                    Some(ChannelMsg::ExitStatus { exit_status: s }) => {
                        exit_status = Some(s);
                    }
                    Some(ChannelMsg::Eof) => {}
                    Some(ChannelMsg::Close) => {
                        // Exit status before close = orderly shutdown.
                        return Ok(if exit_status.is_some() {
                            PumpEnd::Clean(exit_status)
                        } else {
                            PumpEnd::Dropped
                        });
                    }
                    Some(_) => {}
                    None => return Ok(PumpEnd::Dropped),
                }
            }
        }
    }
}

/// Reads local stdin on a plain thread (killed by process exit, so a pending
/// blocking read can never hang runtime shutdown) and forwards bytes.
fn spawn_stdin_reader() -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 8192];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> Result<Self> {
        crossterm::terminal::enable_raw_mode().context("enabling raw terminal mode")?;
        Ok(RawModeGuard)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}
