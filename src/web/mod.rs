use anyhow::{Context as _, Result, bail};
use askama::Template;
use axum::Router;
use axum::extract::{Path, Query};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;

use crate::recording::asciicast;
use crate::session::sessions::one_line;
use crate::{config, store};

pub async fn serve(bind: &str, port: u16, allow_remote: bool) -> Result<()> {
    let ip: std::net::IpAddr = bind.parse().with_context(|| format!("invalid bind address {bind}"))?;
    if !ip.is_loopback() && !allow_remote {
        bail!(
            "refusing to bind {bind}: the web UI has no authentication and exposes session \
             recordings. Pass --allow-remote if you really mean it."
        );
    }

    let app = Router::new()
        .route("/", get(index))
        .route("/sessions", get(sessions_list))
        .route("/sessions/{id}", get(session_detail))
        .route("/sessions/{id}/cast", get(session_cast))
        .route("/sessions/{id}/text", get(session_text))
        .route("/assets/player.min.js", get(player_js))
        .route("/assets/player.css", get(player_css));

    let listener = tokio::net::TcpListener::bind((ip, port))
        .await
        .with_context(|| format!("binding {bind}:{port}"))?;
    eprintln!("agentssh: web UI at http://{bind}:{port}/");
    axum::serve(listener, app).await?;
    Ok(())
}

struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("error: {:#}", self.0)).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        AppError(e.into())
    }
}

struct ContextRow {
    name: String,
    host: String,
    user: String,
}

struct SessionRow {
    id: String,
    context: String,
    kind: String,
    status: String,
    started: String,
    ended: String,
    duration: String,
    exit: String,
    /// The command as it was run, newlines intact — for the detail page.
    command: String,
    /// The same thing collapsed onto one line, for a table cell. Remote
    /// commands are routinely multi-line scripts and a listing that prints them
    /// whole stops being a listing.
    summary: String,
}

struct CommandRow {
    seq: i64,
    started: String,
    exit: String,
    failed: bool,
    command: String,
}

struct SegmentRow {
    connected: String,
    disconnected: String,
    reason: String,
}

fn short(ts: &str) -> String {
    ts.get(..19).unwrap_or(ts).replace('T', " ")
}

fn duration_of(s: &store::Session) -> String {
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;
    let (Some(end), Ok(start)) = (
        s.ended_at.as_deref(),
        OffsetDateTime::parse(&s.started_at, &Rfc3339),
    ) else {
        return "-".into();
    };
    let Ok(end) = OffsetDateTime::parse(end, &Rfc3339) else {
        return "-".into();
    };
    let secs = (end - start).whole_seconds().max(0);
    if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

fn session_row(conn: &rusqlite::Connection, s: &store::Session) -> SessionRow {
    // A persistent shell has no single command; it has a run of them, so the
    // listing shows how many and the most recent one.
    let (command, summary) = if s.kind == "shell" {
        let count = store::command_count(conn, &s.id).unwrap_or(0);
        let last = store::last_command(conn, &s.id).ok().flatten();
        let summary = match &last {
            Some(c) => format!("[{count} cmds] {}", one_line(&c.command, 100)),
            None => "[0 cmds]".into(),
        };
        (last.map(|c| c.command).unwrap_or_default(), summary)
    } else {
        let full = s.command.clone().unwrap_or_default();
        let summary = one_line(&full, 120);
        (full, summary)
    };
    SessionRow {
        id: s.id.clone(),
        context: s.context.clone(),
        kind: s.kind.clone(),
        status: s.status.clone(),
        started: short(&s.started_at),
        ended: s.ended_at.as_deref().map(short).unwrap_or_default(),
        duration: duration_of(s),
        exit: s.exit_code.map(|c| c.to_string()).unwrap_or_default(),
        command,
        summary,
    }
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexPage {
    contexts: Vec<ContextRow>,
    sessions: Vec<SessionRow>,
}

async fn index() -> Result<Html<String>, AppError> {
    let contexts = config::load_contexts()?
        .contexts
        .into_iter()
        .map(|(name, c)| ContextRow {
            name,
            host: format!("{}:{}", c.host, c.port),
            user: c.user,
        })
        .collect();
    let conn = store::open()?;
    let sessions = store::list_sessions(&conn, None, false, 10)?
        .iter()
        .map(|s| session_row(&conn, s))
        .collect();
    Ok(Html(IndexPage { contexts, sessions }.render()?))
}

#[derive(Deserialize)]
struct SessionsQuery {
    context: Option<String>,
}

#[derive(Template)]
#[template(path = "sessions.html")]
struct SessionsPage {
    filter: Option<String>,
    sessions: Vec<SessionRow>,
}

async fn sessions_list(Query(q): Query<SessionsQuery>) -> Result<Html<String>, AppError> {
    let conn = store::open()?;
    let sessions = store::list_sessions(&conn, q.context.as_deref(), false, 200)?
        .iter()
        .map(|s| session_row(&conn, s))
        .collect();
    Ok(Html(SessionsPage { filter: q.context, sessions }.render()?))
}

#[derive(Template)]
#[template(path = "session.html")]
struct SessionPage {
    s: SessionRow,
    segments: Vec<SegmentRow>,
    commands: Vec<CommandRow>,
}

async fn session_detail(Path(id): Path<String>) -> Result<Html<String>, AppError> {
    let conn = store::open()?;
    let s = store::find_session(&conn, &id)?;
    let segments = store::segments_for(&conn, &s.id)?
        .into_iter()
        .map(|seg| SegmentRow {
            connected: short(&seg.connected_at),
            disconnected: seg.disconnected_at.as_deref().map(short).unwrap_or_else(|| "...".into()),
            reason: seg.reason.unwrap_or_else(|| "active".into()),
        })
        .collect();
    let commands = store::commands_for(&conn, &s.id)?
        .into_iter()
        .map(|c| CommandRow {
            seq: c.seq,
            started: short(&c.started_at),
            exit: match c.exit_code {
                Some(e) => e.to_string(),
                None if c.ended_at.is_some() => "-".into(),
                None => "running".into(),
            },
            failed: c.exit_code.is_some_and(|e| e != 0),
            command: c.command,
        })
        .collect();
    let mut row = session_row(&conn, &s);
    // The detail page shows the session's own command, not a shell's last one.
    if s.kind == "shell" {
        row.command = String::new();
    }
    row.ended = s.ended_at.as_deref().map(short).unwrap_or_default();
    Ok(Html(SessionPage { s: row, segments, commands }.render()?))
}

/// The recording as a readable transcript. The player is the right way to watch
/// a session; this is the right way to read or search one.
async fn session_text(Path(id): Path<String>) -> Result<Response, AppError> {
    let conn = store::open()?;
    let s = store::find_session(&conn, &id)?;
    let events = asciicast::read(std::path::Path::new(&s.recording))?;
    Ok((
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        asciicast::render_plain_text(&events),
    )
        .into_response())
}

async fn session_cast(Path(id): Path<String>) -> Result<Response, AppError> {
    let conn = store::open()?;
    let s = store::find_session(&conn, &id)?;
    let bytes = tokio::fs::read(&s.recording).await?;
    Ok((
        [(header::CONTENT_TYPE, "application/x-asciicast")],
        bytes,
    )
        .into_response())
}

async fn player_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript")],
        include_bytes!("../../assets/asciinema-player.min.js").as_slice(),
    )
}

async fn player_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css")],
        include_bytes!("../../assets/asciinema-player.css").as_slice(),
    )
}
