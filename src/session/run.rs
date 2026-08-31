use anyhow::{Context as _, Result};

use crate::recording::asciicast::{EventKind, Writer};
use crate::{config, ssh, store};

/// `agentssh run <context> -- cmd...`: one-shot exec, fully audited.
/// Returns the process exit code for the local process (remote code, or 255 on
/// transport-level failure, matching ssh convention).
pub async fn run(context_name: &str, command: &[String], timeout: Option<u64>) -> Result<i32> {
    let ctx = config::get_context(context_name)?;
    let command_line = shell_join(command);
    let conn = store::open()?;

    let id = store::new_session_id();
    let recording = store::recording_path(&id)?;
    let (cols, rows) = crate::session::interactive::term_size();
    let session = store::Session {
        id: id.clone(),
        context: context_name.to_string(),
        kind: "run".into(),
        command: Some(command_line.clone()),
        tmux_name: None,
        cols: Some(cols as u32),
        rows: Some(rows as u32),
        started_at: store::now_rfc3339(),
        ended_at: None,
        exit_code: None,
        status: "active".into(),
        recording: recording.to_string_lossy().into_owned(),
    };

    let mut recorder = Writer::create(
        &recording,
        cols as u32,
        rows as u32,
        &format!("{context_name}: {command_line}"),
    )?;
    store::create_session(&conn, &session)?;
    recorder.event(EventKind::Marker, &format!("run: {command_line}"))?;

    let result = async {
        let ssh = ssh::connect(&ctx).await?;
        let seg = store::start_segment(&conn, &id)?;
        let out = ssh::exec::run(&ssh, &command_line, &mut recorder).await;
        store::end_segment(&conn, seg, if out.is_ok() { "closed" } else { "dropped" })?;
        out
    };

    let outcome = match timeout {
        Some(secs) => {
            match tokio::time::timeout(std::time::Duration::from_secs(secs), result).await {
                Ok(r) => r,
                Err(_) => {
                    recorder.event(EventKind::Marker, &format!("timed out after {secs}s"))?;
                    store::finish_session(&conn, &id, "closed", None)?;
                    eprintln!("agentssh: command timed out after {secs}s (session {id})");
                    return Ok(255);
                }
            }
        }
        None => result.await,
    };

    match outcome {
        Ok(res) => {
            let code = res.exit_code.map(|c| c as i32).unwrap_or(255);
            store::finish_session(&conn, &id, "closed", Some(code as i64))?;
            Ok(code)
        }
        Err(e) => {
            store::finish_session(&conn, &id, "closed", None)?;
            Err(e).context(format!("session {id}"))
        }
    }
}

/// Join argv into a single remote command line. The SSH protocol has no argv,
/// only a string, so arguments are shell-escaped for the remote shell.
pub fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|a| shell_escape(a))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_escape(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}
