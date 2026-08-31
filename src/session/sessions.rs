use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result};

use crate::cli::ExportFormat;
use crate::recording::asciicast;
use crate::store;

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
    for s in sessions {
        println!(
            "{:<18} {:<12} {:<8} {:<9} {:<21} {:<5} {}",
            s.id,
            s.context,
            s.kind,
            s.status,
            s.started_at.get(..19).unwrap_or(&s.started_at),
            s.exit_code.map(|c| c.to_string()).unwrap_or_default(),
            s.command.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

pub fn show(prefix: &str, tail_chars: usize) -> Result<()> {
    let conn = store::open()?;
    let s = store::find_session(&conn, prefix)?;
    println!("session   {}", s.id);
    println!("context   {}", s.context);
    println!("kind      {}", s.kind);
    println!("status    {}", s.status);
    if let Some(cmd) = &s.command {
        println!("command   {cmd}");
    }
    println!("started   {}", s.started_at);
    if let Some(e) = &s.ended_at {
        println!("ended     {e}");
    }
    if let Some(c) = s.exit_code {
        println!("exit      {c}");
    }
    println!("recording {}", s.recording);

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
