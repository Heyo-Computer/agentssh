//! Persistent remote shells.
//!
//! `run` opens a fresh SSH connection, execs one command, and forgets
//! everything: a `cd` in one call is invisible to the next. A **shell session**
//! keeps a real login shell alive on the remote inside `tmux`, and every
//! `agentssh exec` types one command into it — so working directory, exported
//! variables, an activated virtualenv, and shell history all carry across
//! calls, and the whole sequence replays as a single recording.
//!
//! ## How a command gets run
//!
//! The obvious implementation — `tmux send-keys` with the user's command text —
//! is a quoting minefield and scraping the pane back is worse. Instead:
//!
//! 1. The command text is written to `<workdir>/NNNN.cmd` over an exec channel
//!    **as stdin**, so no quoting layer ever touches it.
//! 2. A fixed, escape-free line is typed into the shell:
//!    `{ . <workdir>/NNNN.cmd ; } > NNNN.out 2>&1 ; echo $? > NNNN.rc`.
//!    Sourcing runs the command *in* the live shell (so `cd` sticks) while the
//!    redirection captures its output exactly, away from the pane's prompts and
//!    echo. The `.rc` file appearing is the completion signal.
//! 3. A second exec channel blocks until `.rc` exists, prints `.out`, and exits
//!    with the captured status — one round trip that returns both the output
//!    and the real exit code.
//!
//! Commands therefore run without a TTY, exactly like `run`: stdout and stderr
//! are merged (that is what the recording shows), and nothing can read stdin.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};

use crate::recording::asciicast::{EventKind, Writer};
use crate::remote::tmux;
use crate::session::run::{shell_escape, shell_join};
use crate::ssh::exec;
use crate::{config, ssh, store};

/// Geometry of the remote tmux pane. Commands redirect their output to a file,
/// so this never wraps anything they print — it only decides how the
/// bookkeeping lines look to a human who attaches to watch the agent work.
/// The *recording* is sized separately, for legible replay.
const PANE_COLS: u32 = 200;
const PANE_ROWS: u32 = 50;

/// How long to wait for a command to give up after being interrupted, before
/// declaring the shell stuck.
const INTERRUPT_GRACE: Duration = Duration::from_secs(5);

/// Per-session scratch space on the remote. The id is 16 hex characters, so
/// this path never needs quoting.
fn workdir(session_id: &str) -> String {
    format!("/tmp/agentssh.{session_id}")
}

/// `agentssh exec <context> -- cmd...`: run one command in the context's
/// persistent shell, starting one if there isn't a live one.
pub async fn exec(
    context_name: &str,
    command: &[String],
    timeout: Option<u64>,
    fresh: bool,
) -> Result<i32> {
    let ctx = config::get_context(context_name)?;
    let command_line = shell_join(command);
    let conn = store::open()?;
    let ssh = ssh::connect(&ctx).await?;
    let session = ensure_shell(&conn, &ssh, &ctx, context_name, fresh).await?;
    run_in_shell(&conn, &ssh, &session, &command_line, timeout).await
}

/// `agentssh shell start <context>`: pre-open a shell without running anything.
pub async fn start(context_name: &str) -> Result<i32> {
    let ctx = config::get_context(context_name)?;
    let conn = store::open()?;
    let ssh = ssh::connect(&ctx).await?;
    if let Some(s) = live_shell(&conn, &ssh, context_name).await? {
        println!("shell {} already open on {context_name}", s.id);
        return Ok(0);
    }
    let s = start_shell(&conn, &ssh, &ctx, context_name).await?;
    println!("shell {} open on {context_name}", s.id);
    Ok(0)
}

pub fn list(all: bool) -> Result<i32> {
    let conn = store::open()?;
    let shells = store::list_shells(&conn, !all)?;
    if shells.is_empty() {
        println!("no persistent shells (one opens on the first: agentssh exec <context> -- ...)");
        return Ok(0);
    }
    println!(
        "{:<18} {:<12} {:<9} {:<21} {:<5} LAST COMMAND",
        "ID", "CONTEXT", "STATUS", "STARTED", "CMDS"
    );
    for s in shells {
        let count = store::command_count(&conn, &s.id)?;
        let last = store::last_command(&conn, &s.id)?
            .map(|c| crate::session::sessions::one_line(&c.command, 46))
            .unwrap_or_else(|| "-".into());
        println!(
            "{:<18} {:<12} {:<9} {:<21} {:<5} {}",
            s.id,
            s.context,
            s.status,
            s.started_at.get(..19).unwrap_or(&s.started_at),
            count,
            last
        );
    }
    Ok(0)
}

/// `agentssh shell stop [target|--all]`: end shells and clean up the remote.
pub async fn stop(target: Option<&str>, all: bool) -> Result<i32> {
    let conn = store::open()?;
    let targets: Vec<store::Session> = match (target, all) {
        (_, true) => store::list_shells(&conn, true)?,
        (Some(t), false) => vec![resolve(&conn, t)?],
        (None, false) => bail!("name a context or session id, or pass --all"),
    };
    if targets.is_empty() {
        println!("no open shells");
        return Ok(0);
    }
    for s in targets {
        let ctx = config::get_context(&s.context)?;
        match ssh::connect(&ctx).await {
            Ok(ssh) => {
                teardown(&ssh, &s).await;
                println!("stopped shell {} on {}", s.id, s.context);
            }
            Err(e) => {
                // The local record must still close, or `exec` keeps trying to
                // reuse a shell on a host we cannot even reach.
                eprintln!("agentssh: could not reach {} to clean up: {e:#}", s.context);
                println!("closed shell {} locally (remote tmux may still exist)", s.id);
            }
        }
        finish(&conn, &s, "closed")?;
    }
    Ok(0)
}

/// `agentssh shell interrupt <target>`: Ctrl-C whatever the shell is running.
pub async fn interrupt(target: &str) -> Result<i32> {
    let conn = store::open()?;
    let s = resolve(&conn, target)?;
    let ctx = config::get_context(&s.context)?;
    let ssh = ssh::connect(&ctx).await?;
    let name = s.tmux_name.clone().unwrap_or_default();
    if !tmux::session_exists(&ssh, &name).await.unwrap_or(false) {
        finish(&conn, &s, "closed")?;
        bail!("shell {} is no longer running on {}", s.id, s.context);
    }
    send_interrupt(&ssh, &name).await?;
    let freed = wait_until_free(&ssh, &name, &workdir(&s.id), &last_token(&conn, &s)?, INTERRUPT_GRACE).await;
    reap_unfinished(&conn, &ssh, &s).await?;
    if freed {
        println!("interrupted shell {} on {}", s.id, s.context);
    } else {
        println!(
            "sent SIGINT to shell {} on {}, but it is still busy — \
             `agentssh shell stop {}` will end it outright",
            s.id, s.context, s.context
        );
    }
    Ok(0)
}

/// Resolve a `shell` subcommand target: a context name, or a session id prefix.
fn resolve(conn: &rusqlite::Connection, target: &str) -> Result<store::Session> {
    if let Some(s) = store::find_live_shell(conn, target)? {
        return Ok(s);
    }
    let s = store::find_session(conn, target)
        .with_context(|| format!("no open shell for context '{target}', and no session id matches"))?;
    if s.kind != "shell" {
        bail!("session {} is a '{}' session, not a persistent shell", s.id, s.kind);
    }
    Ok(s)
}

// ---------------------------------------------------------------------------
// lifecycle
// ---------------------------------------------------------------------------

/// The live shell for a context, confirmed to still exist on the remote.
/// A shell whose tmux session is gone (host rebooted, `/tmp` swept, someone
/// typed `exit`) is closed out here rather than surfacing as a hang later.
async fn live_shell(
    conn: &rusqlite::Connection,
    ssh: &ssh::Ssh,
    context: &str,
) -> Result<Option<store::Session>> {
    let Some(s) = store::find_live_shell(conn, context)? else {
        return Ok(None);
    };
    let name = s.tmux_name.clone().unwrap_or_default();
    if !tmux::session_exists(ssh, &name).await.unwrap_or(false) {
        finish(conn, &s, "closed")?;
        return Ok(None);
    }
    // `attach` leaves the row 'detached'; reusing it makes it current again.
    if s.status != "active" {
        store::set_status(conn, &s.id, "active")?;
    }
    Ok(Some(s))
}

async fn ensure_shell(
    conn: &rusqlite::Connection,
    ssh: &ssh::Ssh,
    ctx: &config::SshContext,
    context: &str,
    fresh: bool,
) -> Result<store::Session> {
    if fresh {
        if let Some(s) = live_shell(conn, ssh, context).await? {
            teardown(ssh, &s).await;
            finish(conn, &s, "closed")?;
        }
    } else if let Some(s) = live_shell(conn, ssh, context).await? {
        return Ok(s);
    }
    start_shell(conn, ssh, ctx, context).await
}

async fn start_shell(
    conn: &rusqlite::Connection,
    ssh: &ssh::Ssh,
    ctx: &config::SshContext,
    context: &str,
) -> Result<store::Session> {
    tmux::require_tmux(ssh, &ctx.host).await?;

    let id = store::new_session_id();
    let recording = store::recording_path(&id)?;
    let name = tmux::session_name(&id);
    let dir = workdir(&id);

    // The pane is wide so a human attaching sees unwrapped bookkeeping lines,
    // but nothing a command prints is wrapped at that width — output is
    // redirected to a file, never through the pane. So the recording is sized
    // for reading instead, the same way `run` sizes its own.
    let (rec_cols, rec_rows) = crate::session::record_size();
    let session = store::Session {
        id: id.clone(),
        context: context.to_string(),
        kind: "shell".into(),
        command: None,
        tmux_name: Some(name.clone()),
        cols: Some(rec_cols as u32),
        rows: Some(rec_rows as u32),
        started_at: store::now_rfc3339(),
        ended_at: None,
        exit_code: None,
        status: "active".into(),
        recording: recording.to_string_lossy().into_owned(),
    };

    // Pin the pane to a POSIX shell: a login `fish` or `csh` would not
    // understand the wrapper line that every command rides in on.
    let boot = format!(
        "mkdir -p {dir} && chmod 700 {dir} && \
         tmux new-session -d -s {name} -x {PANE_COLS} -y {PANE_ROWS} \
         'if command -v bash >/dev/null 2>&1; then exec bash -l; else exec sh; fi'"
    );
    let out = exec::capture(ssh, &boot, None).await?;
    if !out.ok() {
        bail!("could not start a remote shell on {}: {}", ctx.host, out.err());
    }

    Writer::create(&recording, rec_cols as u32, rec_rows as u32, &format!("{context} (shell)"))?
        .event(EventKind::Marker, &format!("shell opened on {context}"))?;
    store::create_session(conn, &session)?;
    eprintln!(
        "agentssh: opened persistent shell {} on {context} (watch it with: agentssh attach {})",
        id,
        &id[..6]
    );
    Ok(session)
}

/// Kill the remote tmux session and remove its scratch directory. Best effort:
/// a shell we cannot clean up must still close locally.
async fn teardown(ssh: &ssh::Ssh, s: &store::Session) {
    let name = s.tmux_name.clone().unwrap_or_default();
    let dir = workdir(&s.id);
    let _ = exec::capture(
        ssh,
        &format!("tmux kill-session -t {name} 2>/dev/null; rm -rf {dir}"),
        None,
    )
    .await;
}

/// Close a shell's audit record, noting the end in its recording.
fn finish(conn: &rusqlite::Connection, s: &store::Session, reason: &str) -> Result<()> {
    if let Ok(mut rec) = Writer::append(Path::new(&s.recording), &s.started_at) {
        let _ = rec.event(EventKind::Marker, &format!("shell {reason}"));
    }
    store::finish_session(conn, &s.id, "closed", None)
}

// ---------------------------------------------------------------------------
// running one command
// ---------------------------------------------------------------------------

async fn run_in_shell(
    conn: &rusqlite::Connection,
    ssh: &ssh::Ssh,
    session: &store::Session,
    command_line: &str,
    timeout: Option<u64>,
) -> Result<i32> {
    let name = session.tmux_name.clone().unwrap_or_default();
    let dir = workdir(&session.id);

    // A command we stopped waiting for may have finished since; collect it
    // before deciding the shell is busy.
    if let Some(busy) = reap_unfinished(conn, ssh, session).await? {
        bail!(
            "shell {} on {} is still running command {} ({}).\n\
             Wait for it, interrupt it with `agentssh shell interrupt {}`, or run this \
             command on its own connection with `agentssh run`.",
            &session.id[..8],
            session.context,
            busy.seq,
            crate::session::sessions::one_line(&busy.command, 60),
            session.context
        );
    }

    let seq = store::next_command_seq(conn, &session.id)?;
    let token = format!("{seq:04}");
    let mut rec = Writer::append(Path::new(&session.recording), &session.started_at)?
        .newline_fixup(true);

    // Stage the command as data, not as argv — no quoting layer touches it.
    let staged = exec::capture(
        ssh,
        &format!("mkdir -p {dir} && chmod 700 {dir} && cat > {dir}/{token}.cmd"),
        Some(command_line.as_bytes()),
    )
    .await?;
    if !staged.ok() {
        bail!("could not stage the command in {dir} on {}: {}", session.context, staged.err());
    }

    rec.event(EventKind::Marker, &format!("$ {}", crate::session::sessions::one_line(command_line, 120)))?;
    rec.text(&prompt_block(command_line))?;

    let wrapper = format!(
        "{{ . {dir}/{token}.cmd ; }} > {dir}/{token}.out 2>&1 ; echo $? > {dir}/{token}.rc"
    );
    let sent = exec::capture(
        ssh,
        &format!(
            "tmux send-keys -t {name} -l {} && tmux send-keys -t {name} Enter",
            shell_escape(&wrapper)
        ),
        None,
    )
    .await?;
    if !sent.ok() {
        bail!("could not type into shell {} on {}: {}", &session.id[..8], session.context, sent.err());
    }

    let row = store::start_command(conn, &session.id, seq, command_line)?;
    let waiter = wait_script(&dir, &token, &name);

    let outcome = match timeout {
        Some(secs) => {
            match tokio::time::timeout(
                Duration::from_secs(secs),
                crate::ssh::exec::run(ssh, &waiter, &mut rec),
            )
            .await
            {
                Ok(r) => r,
                Err(_) => {
                    return timed_out(conn, ssh, session, &mut rec, row, &token, secs).await;
                }
            }
        }
        None => crate::ssh::exec::run(ssh, &waiter, &mut rec).await,
    };

    match outcome {
        Ok(res) => {
            let code = res.exit_code.map(|c| c as i32).unwrap_or(255);
            // 254 is the wait script's own signal that the shell died under it;
            // the missing .rc file is what distinguishes it from a command that
            // genuinely exited 254.
            if code == 254 && !rc_exists(ssh, &dir, &token).await {
                store::finish_command(conn, row, None)?;
                finish(conn, session, "ended while a command was running")?;
                bail!(
                    "the persistent shell on {} ended while the command was running; \
                     the next `agentssh exec` will open a fresh one",
                    session.context
                );
            }
            footer(&mut rec, code)?;
            store::finish_command(conn, row, Some(code as i64))?;
            Ok(code)
        }
        Err(e) => {
            // Transport died mid-wait. The command itself is still running in
            // tmux and will be collected on the next call.
            rec.ensure_line_start()?;
            rec.event(EventKind::Marker, "connection lost while waiting")?;
            Err(e).with_context(|| {
                format!(
                    "lost the connection to {} while waiting; the command is still running in \
                     shell {} and its result will be picked up on the next call",
                    session.context,
                    &session.id[..8]
                )
            })
        }
    }
}

/// Timeout path: interrupt the command the way a person at the keyboard would,
/// then salvage whatever it printed so the audit trail is not left empty.
async fn timed_out(
    conn: &rusqlite::Connection,
    ssh: &ssh::Ssh,
    session: &store::Session,
    rec: &mut Writer,
    row: i64,
    token: &str,
    secs: u64,
) -> Result<i32> {
    let name = session.tmux_name.clone().unwrap_or_default();
    let dir = workdir(&session.id);
    send_interrupt(ssh, &name).await.ok();
    let freed = wait_until_free(ssh, &name, &dir, token, INTERRUPT_GRACE).await;

    // Whatever the command managed to print before it was cut off belongs in
    // the transcript, and in front of the caller.
    let partial = exec::capture(ssh, &format!("cat {dir}/{token}.out 2>/dev/null"), None).await?;
    if !partial.stdout.is_empty() {
        use tokio::io::AsyncWriteExt as _;
        let mut stdout = tokio::io::stdout();
        stdout.write_all(&partial.stdout).await?;
        stdout.flush().await?;
        rec.bytes(EventKind::Output, &partial.stdout)?;
    }
    rec.ensure_line_start()?;
    rec.text(&format!("\x1b[1;33m[timed out after {secs}s; sent SIGINT]\x1b[0m\n"))?;
    rec.event(EventKind::Marker, &format!("timed out after {secs}s"))?;

    let code = read_rc(ssh, &dir, token).await;
    if freed {
        // An interactive shell throws away the rest of a command line when it
        // takes SIGINT, so the wrapper's `echo $? > .rc` usually never runs:
        // "interrupted, exit code unknown" is the honest record.
        store::finish_command(conn, row, code.map(|c| c as i64))?;
        eprintln!("agentssh: command timed out after {secs}s and was interrupted");
    } else {
        // Leave the row open: the shell is still busy and the next `exec` has
        // to see that rather than typing over a running command.
        eprintln!(
            "agentssh: command timed out after {secs}s and ignored SIGINT; shell {} on {} is \
             still busy (end it with: agentssh shell stop {})",
            &session.id[..8],
            session.context,
            session.context
        );
    }
    Ok(255)
}

/// Wait for the shell to come back to a prompt after an interrupt: either the
/// wrapper survived long enough to record an exit code, or the shell discarded
/// the rest of the line and the pane is idle again.
async fn wait_until_free(
    ssh: &ssh::Ssh,
    name: &str,
    dir: &str,
    token: &str,
    grace: Duration,
) -> bool {
    let deadline = std::time::Instant::now() + grace;
    loop {
        if rc_exists(ssh, dir, token).await || pane_idle(ssh, name).await {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Is the shell sitting at a prompt? `pane_current_command` names the
/// foreground command on the pane's tty, which is the shell itself when
/// nothing is running.
async fn pane_idle(ssh: &ssh::Ssh, name: &str) -> bool {
    let out = match exec::capture(
        ssh,
        &format!("tmux display-message -p -t {name} '#{{pane_current_command}}'"),
        None,
    )
    .await
    {
        Ok(o) if o.ok() => o.out(),
        _ => return false,
    };
    matches!(
        out.trim_start_matches('-'),
        "bash" | "sh" | "zsh" | "dash" | "ksh" | "ash"
    )
}

/// Blocks on the remote until the command's exit-code file appears, prints
/// everything the command wrote, and exits with the command's own status — so
/// one channel returns both halves of the result.
fn wait_script(dir: &str, token: &str, name: &str) -> String {
    format!(
        "if sleep 0.1 2>/dev/null; then s=0.1; else s=1; fi; i=0; \
         while [ ! -f {dir}/{token}.rc ]; do \
           i=$((i+1)); \
           if [ $((i % 100)) -eq 0 ] && ! tmux has-session -t {name} 2>/dev/null; then \
             echo 'agentssh: the remote shell ended while this command was running' >&2; \
             exit 254; \
           fi; \
           sleep $s; \
         done; \
         cat {dir}/{token}.out 2>/dev/null; exit $(cat {dir}/{token}.rc)"
    )
}

async fn send_interrupt(ssh: &ssh::Ssh, name: &str) -> Result<()> {
    let out = exec::capture(ssh, &format!("tmux send-keys -t {name} C-c"), None).await?;
    if !out.ok() {
        bail!("could not interrupt the remote shell: {}", out.err());
    }
    Ok(())
}

async fn rc_exists(ssh: &ssh::Ssh, dir: &str, token: &str) -> bool {
    matches!(
        exec::probe(ssh, &format!("test -f {dir}/{token}.rc")).await,
        Ok(Some(0))
    )
}

async fn read_rc(ssh: &ssh::Ssh, dir: &str, token: &str) -> Option<i32> {
    let out = exec::capture(ssh, &format!("cat {dir}/{token}.rc 2>/dev/null"), None)
        .await
        .ok()?;
    out.out().parse::<i32>().ok()
}

/// A command whose result we stopped waiting for leaves an open row. If it has
/// finished since, fold its output and exit code into the record; if it has
/// not, hand it back so the caller can refuse to type over a busy shell.
async fn reap_unfinished(
    conn: &rusqlite::Connection,
    ssh: &ssh::Ssh,
    session: &store::Session,
) -> Result<Option<store::Command>> {
    let Some(last) = store::last_command(conn, &session.id)? else {
        return Ok(None);
    };
    if last.ended_at.is_some() {
        return Ok(None);
    }
    let dir = workdir(&session.id);
    let token = format!("{:04}", last.seq);
    let Some(code) = read_rc(ssh, &dir, &token).await else {
        let name = session.tmux_name.clone().unwrap_or_default();
        if !pane_idle(ssh, &name).await {
            return Ok(Some(last));
        }
        // Idle with no exit code: the command was interrupted and the shell
        // dropped the rest of the wrapper. Close the row so the shell is
        // usable again, but never invent an exit code for it.
        let id = command_row_id(conn, &session.id, last.seq)?;
        store::finish_command(conn, id, None)?;
        return Ok(None);
    };

    let out = exec::capture(ssh, &format!("cat {dir}/{token}.out 2>/dev/null"), None).await?;
    let mut rec = Writer::append(Path::new(&session.recording), &session.started_at)?
        .newline_fixup(true);
    rec.event(EventKind::Marker, &format!("late result for command {}", last.seq))?;
    if !out.stdout.is_empty() {
        rec.bytes(EventKind::Output, &out.stdout)?;
    }
    footer(&mut rec, code)?;

    let id = command_row_id(conn, &session.id, last.seq)?;
    store::finish_command(conn, id, Some(code as i64))?;
    eprintln!(
        "agentssh: collected the result of command {} (exit {code}), which had timed out",
        last.seq
    );
    Ok(None)
}

fn command_row_id(conn: &rusqlite::Connection, session_id: &str, seq: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT id FROM commands WHERE session_id = ?1 AND seq = ?2",
        rusqlite::params![session_id, seq],
        |r| r.get(0),
    )?)
}

/// The token of the most recent command, for the interrupt path.
fn last_token(conn: &rusqlite::Connection, session: &store::Session) -> Result<String> {
    Ok(match store::last_command(conn, &session.id)? {
        Some(c) => format!("{:04}", c.seq),
        None => "0000".into(),
    })
}

/// Echo the command into the recording the way a shell would, so the replay
/// reads as a session rather than as disembodied output.
fn prompt_block(command: &str) -> String {
    let mut out = String::new();
    let mut lines = command.lines();
    match lines.next() {
        Some(first) => out.push_str(&format!("\x1b[1;36m$\x1b[0m {first}\n")),
        None => return "\x1b[1;36m$\x1b[0m\n".into(),
    }
    for line in lines {
        out.push_str(&format!("\x1b[1;36m>\x1b[0m {line}\n"));
    }
    out
}

/// Only failures get a footer — a clean command should replay as clean as it
/// looked in a real terminal.
fn footer(rec: &mut Writer, code: i32) -> Result<()> {
    rec.event(EventKind::Marker, &format!("exit {code}"))?;
    if code == 0 {
        return Ok(());
    }
    rec.ensure_line_start()?;
    rec.text(&format!("\x1b[1;31m[exit {code}]\x1b[0m\n"))
}
