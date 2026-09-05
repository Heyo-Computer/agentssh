use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::config;

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub context: String,
    pub kind: String, // run | exec (persistent shell) | connect
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

/// One command run inside a persistent shell session. A `shell` session's
/// recording is a single asciicast spanning every command; this table is what
/// makes the individual commands (and their exit codes) auditable.
#[derive(Debug, Clone)]
pub struct Command {
    pub seq: i64,
    pub command: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub exit_code: Option<i64>,
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
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    Ok(conn)
}

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS sessions (
      id          TEXT PRIMARY KEY,
      context     TEXT NOT NULL,
      kind        TEXT NOT NULL CHECK (kind IN ('run','shell','connect')),
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
    CREATE INDEX IF NOT EXISTS idx_segments_session ON segments(session_id);
    CREATE TABLE IF NOT EXISTS commands (
      id INTEGER PRIMARY KEY,
      session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
      seq         INTEGER NOT NULL,
      command     TEXT NOT NULL,
      started_at  TEXT NOT NULL,
      ended_at    TEXT,
      exit_code   INTEGER
    );
    CREATE INDEX IF NOT EXISTS idx_commands_session ON commands(session_id, seq);
";

/// Databases written before persistent shells existed carry a
/// `CHECK (kind IN ('run','connect'))` that would reject a `shell` row. SQLite
/// cannot alter a constraint in place, so rebuild the table when we see the old
/// one. Everything else is additive and handled by `CREATE TABLE IF NOT EXISTS`.
fn migrate(conn: &Connection) -> Result<()> {
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'sessions'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let Some(sql) = sql else { return Ok(()) };
    if sql.contains("'shell'") {
        return Ok(());
    }
    conn.execute_batch(
        "PRAGMA foreign_keys = OFF;
         BEGIN;
         CREATE TABLE sessions_migrated (
           id          TEXT PRIMARY KEY,
           context     TEXT NOT NULL,
           kind        TEXT NOT NULL CHECK (kind IN ('run','shell','connect')),
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
         INSERT INTO sessions_migrated
           SELECT id, context, kind, command, tmux_name, cols, rows,
                  started_at, ended_at, exit_code, status, recording
           FROM sessions;
         DROP TABLE sessions;
         ALTER TABLE sessions_migrated RENAME TO sessions;
         COMMIT;",
    )
    .context("migrating the sessions table to allow persistent shell sessions")?;
    Ok(())
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
    conn.execute("DELETE FROM commands WHERE session_id = ?1", params![id])?;
    conn.execute("DELETE FROM segments WHERE session_id = ?1", params![id])?;
    conn.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
    Ok(())
}

/// The live persistent shell for a context, if there is one. `detached` counts:
/// a user can `attach` to the agent's shell and detach again without ending it.
/// The caller still has to confirm the remote tmux session is really there.
pub fn find_live_shell(conn: &Connection, context: &str) -> Result<Option<Session>> {
    Ok(conn
        .query_row(
            "SELECT * FROM sessions
             WHERE context = ?1 AND kind = 'shell' AND status IN ('active','detached')
             ORDER BY started_at DESC LIMIT 1",
            params![context],
            row_to_session,
        )
        .optional()?)
}

pub fn list_shells(conn: &Connection, live_only: bool) -> Result<Vec<Session>> {
    let sql = if live_only {
        "SELECT * FROM sessions WHERE kind = 'shell' AND status IN ('active','detached')
         ORDER BY started_at DESC"
    } else {
        "SELECT * FROM sessions WHERE kind = 'shell' ORDER BY started_at DESC"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], row_to_session)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn row_to_command(row: &rusqlite::Row) -> rusqlite::Result<Command> {
    Ok(Command {
        seq: row.get("seq")?,
        command: row.get("command")?,
        started_at: row.get("started_at")?,
        ended_at: row.get("ended_at")?,
        exit_code: row.get("exit_code")?,
    })
}

pub fn next_command_seq(conn: &Connection, session_id: &str) -> Result<i64> {
    let max: Option<i64> = conn.query_row(
        "SELECT MAX(seq) FROM commands WHERE session_id = ?1",
        params![session_id],
        |r| r.get(0),
    )?;
    Ok(max.unwrap_or(0) + 1)
}

pub fn start_command(conn: &Connection, session_id: &str, seq: i64, command: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO commands (session_id, seq, command, started_at) VALUES (?1, ?2, ?3, ?4)",
        params![session_id, seq, command, now_rfc3339()],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn finish_command(conn: &Connection, id: i64, exit_code: Option<i64>) -> Result<()> {
    conn.execute(
        "UPDATE commands SET ended_at = ?2, exit_code = ?3 WHERE id = ?1",
        params![id, now_rfc3339(), exit_code],
    )?;
    Ok(())
}

pub fn commands_for(conn: &Connection, session_id: &str) -> Result<Vec<Command>> {
    let mut stmt = conn.prepare(
        "SELECT seq, command, started_at, ended_at, exit_code
         FROM commands WHERE session_id = ?1 ORDER BY seq",
    )?;
    let rows = stmt.query_map(params![session_id], row_to_command)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The most recent command in a shell session. A row with no `ended_at` is one
/// that never came back — the shell is still busy with it.
pub fn last_command(conn: &Connection, session_id: &str) -> Result<Option<Command>> {
    Ok(conn
        .query_row(
            "SELECT seq, command, started_at, ended_at, exit_code
             FROM commands WHERE session_id = ?1 ORDER BY seq DESC LIMIT 1",
            params![session_id],
            row_to_command,
        )
        .optional()?)
}

pub fn command_count(conn: &Connection, session_id: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM commands WHERE session_id = ?1",
        params![session_id],
        |r| r.get(0),
    )?)
}
