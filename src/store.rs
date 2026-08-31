use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::config;

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub context: String,
    pub kind: String, // run | connect
    pub command: Option<String>,
    pub tmux_name: Option<String>,
    pub cols: Option<u32>,
    pub rows: Option<u32>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub exit_code: Option<i64>,
    pub status: String, // active | detached | closed
    pub recording: String,
}

#[derive(Debug, Clone)]
pub struct Segment {
    pub connected_at: String,
    pub disconnected_at: Option<String>,
    pub reason: Option<String>,
}

pub fn now_rfc3339() -> String {
    OffsetDateTime::now_utc().format(&Rfc3339).expect("rfc3339 format")
}

pub fn open() -> Result<Connection> {
    let paths = config::paths()?;
    config::ensure_private_dir(paths.db_file.parent().unwrap())?;
    config::ensure_private_dir(&paths.recordings_dir)?;
    let existed = paths.db_file.exists();
    let conn = Connection::open(&paths.db_file)?;
    if !existed {
        fs::set_permissions(&paths.db_file, fs::Permissions::from_mode(0o600))?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
           id          TEXT PRIMARY KEY,
           context     TEXT NOT NULL,
           kind        TEXT NOT NULL CHECK (kind IN ('run','connect')),
           command     TEXT,
           tmux_name   TEXT,
           cols        INTEGER,
           rows        INTEGER,
           started_at  TEXT NOT NULL,
           ended_at    TEXT,
           exit_code   INTEGER,
           status      TEXT NOT NULL DEFAULT 'active',
           recording   TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS segments (
           id INTEGER PRIMARY KEY,
           session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
           connected_at    TEXT NOT NULL,
           disconnected_at TEXT,
           reason          TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_segments_session ON segments(session_id);",
    )?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    Ok(conn)
}

pub fn new_session_id() -> String {
    use rand::RngExt;
    let bytes: [u8; 8] = rand::rng().random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn recording_path(id: &str) -> Result<PathBuf> {
    Ok(config::paths()?.recordings_dir.join(format!("{id}.cast")))
}

pub fn create_session(conn: &Connection, s: &Session) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions (id, context, kind, command, tmux_name, cols, rows, started_at, status, recording)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![s.id, s.context, s.kind, s.command, s.tmux_name, s.cols, s.rows, s.started_at, s.status, s.recording],
    )?;
    Ok(())
}

pub fn finish_session(conn: &Connection, id: &str, status: &str, exit_code: Option<i64>) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET status = ?2, exit_code = ?3, ended_at = ?4 WHERE id = ?1",
        params![id, status, exit_code, now_rfc3339()],
    )?;
    Ok(())
}

pub fn set_status(conn: &Connection, id: &str, status: &str) -> Result<()> {
    conn.execute("UPDATE sessions SET status = ?2 WHERE id = ?1", params![id, status])?;
    Ok(())
}

pub fn start_segment(conn: &Connection, session_id: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO segments (session_id, connected_at) VALUES (?1, ?2)",
        params![session_id, now_rfc3339()],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn end_segment(conn: &Connection, segment_id: i64, reason: &str) -> Result<()> {
    conn.execute(
        "UPDATE segments SET disconnected_at = ?2, reason = ?3 WHERE id = ?1",
        params![segment_id, now_rfc3339(), reason],
    )?;
    Ok(())
}

fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get("id")?,
        context: row.get("context")?,
        kind: row.get("kind")?,
        command: row.get("command")?,
        tmux_name: row.get("tmux_name")?,
        cols: row.get("cols")?,
        rows: row.get("rows")?,
        started_at: row.get("started_at")?,
        ended_at: row.get("ended_at")?,
        exit_code: row.get("exit_code")?,
        status: row.get("status")?,
        recording: row.get("recording")?,
    })
}

pub fn list_sessions(
    conn: &Connection,
    context: Option<&str>,
    active_only: bool,
    limit: usize,
) -> Result<Vec<Session>> {
    let mut sql = String::from("SELECT * FROM sessions WHERE 1=1");
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(c) = context {
        sql.push_str(" AND context = ?");
        args.push(Box::new(c.to_string()));
    }
    if active_only {
        sql.push_str(" AND status IN ('active','detached')");
    }
    sql.push_str(" ORDER BY started_at DESC LIMIT ?");
    args.push(Box::new(limit as i64));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())), row_to_session)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Find a session by full id or unique prefix (git-style).
pub fn find_session(conn: &Connection, prefix: &str) -> Result<Session> {
    let exact: Option<Session> = conn
        .query_row("SELECT * FROM sessions WHERE id = ?1", params![prefix], row_to_session)
        .optional()?;
    if let Some(s) = exact {
        return Ok(s);
    }
    let mut stmt = conn.prepare("SELECT * FROM sessions WHERE id LIKE ?1 || '%' ORDER BY started_at DESC")?;
    let matches: Vec<Session> = stmt
        .query_map(params![prefix], row_to_session)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    match matches.len() {
        0 => bail!("no session matching '{prefix}' (see: agentssh sessions list)"),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => bail!("'{prefix}' is ambiguous ({n} sessions match); use a longer prefix"),
    }
}

pub fn segments_for(conn: &Connection, session_id: &str) -> Result<Vec<Segment>> {
    let mut stmt = conn.prepare(
        "SELECT connected_at, disconnected_at, reason FROM segments WHERE session_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map(params![session_id], |row| {
        Ok(Segment {
            connected_at: row.get(0)?,
            disconnected_at: row.get(1)?,
            reason: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn delete_session(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM segments WHERE session_id = ?1", params![id])?;
    conn.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
    Ok(())
}
