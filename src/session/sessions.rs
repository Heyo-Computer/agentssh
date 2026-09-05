use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result};

use crate::cli::ExportFormat;
use crate::recording::asciicast;
use crate::store;

/// Squash a command onto one line for a table cell.
///
/// Remote command lines are routinely multi-line `sh -c` scripts. Printed raw
/// into a column they wrap across dozens of rows and destroy the table, so the
/// listing shows a single collapsed line and `sessions show` keeps the full
/// text.
pub fn one_line(command: &str, max: usize) -> String {
    let mut flat = String::with_capacity(command.len().min(max * 2));
    let mut gap = false;
    for c in command.chars() {
        if c.is_whitespace() || c.is_control() {
            gap = !flat.is_empty();
            continue;
        }
        if gap {
            flat.push(' ');
            gap = false;
        }
        flat.push(c);
    }
    if flat.chars().count() <= max {
        return flat;
    }
    let head: String = flat.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// What to show in the COMMAND column. A `run` carries one command line; a
/// persistent shell carries many, so it shows its most recent one and how many
/// have gone through it.
fn list_command(conn: &rusqlite::Connection, s: &store::Session, width: usize) -> Result<String> {
    if s.kind != "shell" {
        return Ok(match &s.command {
            Some(c) => one_line(c, width),
            None => "-".into(),
        });
    }
    let count = store::command_count(conn, &s.id)?;
    Ok(match store::last_command(conn, &s.id)? {
        Some(c) => format!("[{count} cmds] {}", one_line(&c.command, width.saturating_sub(12))),
        None => "[0 cmds]".into(),
    })
}

pub fn list(context: Option<&str>, active: bool, limit: usize) -> Result<()> {
    let conn = store::open()?;
    let sessions = store::list_sessions(&conn, context, active, limit)?;
    if sessions.is_empty() {
        println!("no sessions");
        return Ok(());
    }
    println!(
        "{:<18} {:<12} {:<8} {:<9} {:<21} {:<5} COMMAND",
        "ID", "CONTEXT", "KIND", "STATUS", "STARTED", "EXIT"
    );
    let width = command_column_width();
    for s in sessions {
        println!(
            "{:<18} {:<12} {:<8} {:<9} {:<21} {:<5} {}",
            s.id,
            s.context,
            s.kind,
            s.status,
            s.started_at.get(..19).unwrap_or(&s.started_at),
            s.exit_code.map(|c| c.to_string()).unwrap_or_default(),
            list_command(&conn, &s, width)?
        );
    }
    Ok(())
}

/// Columns left for the command after the fixed ones, clamped so the output is
/// sane both in a wide terminal and with no tty at all.
fn command_column_width() -> usize {
    const FIXED: usize = 18 + 1 + 12 + 1 + 8 + 1 + 9 + 1 + 21 + 1 + 5 + 1;
    let cols = crossterm::terminal::size().map(|(c, _)| c as usize).unwrap_or(0);
    if cols == 0 {
        return 80;
    }
    cols.saturating_sub(FIXED).clamp(24, 200)
}

pub fn show(prefix: &str, tail_chars: usize) -> Result<()> {
    let conn = store::open()?;
    let s = store::find_session(&conn, prefix)?;
    println!("session   {}", s.id);
    println!("context   {}", s.context);
    println!("kind      {}", s.kind);
    println!("status    {}", s.status);
    if let Some(cmd) = &s.command {
        // Keep the full text, but indent continuation lines so a multi-line
        // script still reads as one labelled field.
        let mut lines = cmd.lines();
        println!("command   {}", lines.next().unwrap_or_default());
        for line in lines {
            println!("          {line}");
        }
    }
    println!("started   {}", s.started_at);
    if let Some(e) = &s.ended_at {
        println!("ended     {e}");
    }
    if let Some(c) = s.exit_code {
        println!("exit      {c}");
    }
    println!("recording {}", s.recording);

    let commands = store::commands_for(&conn, &s.id)?;
    if !commands.is_empty() {
        println!("\ncommands:");
        for c in &commands {
            println!(
                "  {:>3} {} {:>4}  {}",
                c.seq,
                c.started_at.get(11..19).unwrap_or(""),
                c.exit_code
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| if c.ended_at.is_some() { "-".into() } else { "...".into() }),
                one_line(&c.command, 90)
            );
        }
    }

    let segments = store::segments_for(&conn, &s.id)?;
    if !segments.is_empty() {
        println!("\nsegments:");
        for (i, seg) in segments.iter().enumerate() {
            println!(
                "  {} {} -> {} ({})",
                i + 1,
                seg.connected_at.get(..19).unwrap_or(&seg.connected_at),
                seg.disconnected_at
                    .as_deref()
                    .map(|d| d.get(..19).unwrap_or(d))
                    .unwrap_or("..."),
                seg.reason.as_deref().unwrap_or("active")
            );
        }
    }

    let path = Path::new(&s.recording);
    if path.exists() {
        let events = asciicast::read(path)?;
        let text = asciicast::render_plain_text(&events);
        let tail: String = if text.len() > tail_chars {
            format!("...{}", &text[text.len() - tail_chars..])
        } else {
            text
        };
        if !tail.trim().is_empty() {
            println!("\noutput tail:\n{tail}");
        }
    }
    Ok(())
}

pub fn export(prefix: &str, format: ExportFormat, output: Option<&str>) -> Result<()> {
    let conn = store::open()?;
    let s = store::find_session(&conn, prefix)?;
    let path = Path::new(&s.recording);
    let content = match format {
        ExportFormat::Asciicast => std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", s.recording))?,
        ExportFormat::Txt => {
            let events = asciicast::read(path)?;
            asciicast::render_plain_text(&events)
        }
    };
    match output {
        Some(f) => std::fs::write(f, content)?,
        None => std::io::stdout().write_all(content.as_bytes())?,
    }
    Ok(())
}

pub fn rm(prefix: &str) -> Result<()> {
    let conn = store::open()?;
    let s = store::find_session(&conn, prefix)?;
    if Path::new(&s.recording).exists() {
        std::fs::remove_file(&s.recording)?;
    }
    store::delete_session(&conn, &s.id)?;
    println!("removed session {}", s.id);
    Ok(())
}
