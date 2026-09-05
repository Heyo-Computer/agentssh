use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{Context as _, Result};
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Event types in asciicast v2: output, input, marker, resize.
#[derive(Copy, Clone)]
pub enum EventKind {
    Output,
    Input,
    Marker,
    Resize,
}

impl EventKind {
    fn code(self) -> &'static str {
        match self {
            EventKind::Output => "o",
            EventKind::Input => "i",
            EventKind::Marker => "m",
            EventKind::Resize => "r",
        }
    }
}

/// Appends asciicast v2 events to a session's .cast file. Elapsed time is
/// measured from the logical session start (wall clock), so a recording that
/// spans reconnects and multiple processes stays monotonic.
pub struct Writer {
    out: BufWriter<fs::File>,
    epoch: OffsetDateTime,
    /// Translate bare LF to CRLF in recorded output. A PTY session already
    /// emits CRLF; an exec channel does not, and a terminal emulator treats a
    /// lone LF as "down one row, keep the column" — so replaying unfixed exec
    /// output staircases every line off the right edge of the screen.
    crlf: bool,
    /// Carried across chunk boundaries so a CR ending one write and an LF
    /// starting the next is not turned into CR CR LF.
    pending_cr: bool,
    /// Whether the cursor is at column 0, so agentssh's own footers can start
    /// on a fresh line without blank-lining output that already ended in one.
    at_line_start: bool,
}

impl Writer {
    /// Create a new recording with its header line.
    pub fn create(path: &Path, cols: u32, rows: u32, title: &str) -> Result<Self> {
        let file = fs::OpenOptions::new()
            .create_new(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("creating recording {}", path.display()))?;
        let epoch = OffsetDateTime::now_utc();
        let mut w = Writer { out: BufWriter::new(file), epoch, crlf: false, pending_cr: false, at_line_start: true };
        let header = json!({
            "version": 2,
            "width": cols,
            "height": rows,
            "timestamp": epoch.unix_timestamp(),
            "title": title,
            "env": {"TERM": std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into())},
        });
        serde_json::to_writer(&mut w.out, &header)?;
        w.out.write_all(b"\n")?;
        w.out.flush()?;
        Ok(w)
    }

    /// Reopen an existing recording for appending; elapsed continues from the
    /// session's original start time (RFC3339).
    pub fn append(path: &Path, started_at: &str) -> Result<Self> {
        let epoch = OffsetDateTime::parse(started_at, &Rfc3339)
            .with_context(|| format!("bad session start time {started_at}"))?;
        let file = fs::OpenOptions::new()
            .append(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("opening recording {}", path.display()))?;
        Ok(Writer { out: BufWriter::new(file), epoch, crlf: false, pending_cr: false, at_line_start: true })
    }

    fn elapsed(&self) -> f64 {
        let d = OffsetDateTime::now_utc() - self.epoch;
        (d.whole_microseconds() as f64 / 1_000_000.0).max(0.0)
    }

    pub fn event(&mut self, kind: EventKind, data: &str) -> Result<()> {
        let line = json!([self.elapsed(), kind.code(), data]);
        serde_json::to_writer(&mut self.out, &line)?;
        self.out.write_all(b"\n")?;
        self.out.flush()?;
        Ok(())
    }

    /// Turn on LF -> CRLF translation for recorded *output*. Set this for
    /// sessions whose remote end has no PTY (`run`, and every command run in a
    /// persistent shell); leave it off for real PTY sessions, which already
    /// send CRLF and would otherwise get doubled carriage returns.
    pub fn newline_fixup(mut self, on: bool) -> Self {
        self.crlf = on;
        self
    }

    /// Record raw terminal bytes (lossy UTF-8 is acceptable for audit purposes;
    /// invalid sequences are replaced, never dropped silently).
    pub fn bytes(&mut self, kind: EventKind, data: &[u8]) -> Result<()> {
        if !matches!(kind, EventKind::Output) {
            return self.event(kind, &String::from_utf8_lossy(data));
        }
        if let Some(&last) = data.last() {
            self.at_line_start = last == b'\n';
        }
        if self.crlf {
            let fixed = self.crlf_encode(data);
            return self.event(kind, &String::from_utf8_lossy(&fixed));
        }
        self.event(kind, &String::from_utf8_lossy(data))
    }

    fn crlf_encode(&mut self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + data.len() / 16);
        for &b in data {
            if b == b'\n' && !self.pending_cr {
                out.push(b'\r');
            }
            self.pending_cr = b == b'\r';
            out.push(b);
        }
        out
    }

    /// Write text agentssh itself generates into the output stream (the echoed
    /// command line of a shell session, an exit-code footer). Newlines are
    /// written as CRLF unconditionally: this text is never PTY output.
    pub fn text(&mut self, data: &str) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let fixed = data.replace("\r\n", "\n").replace('\n', "\r\n");
        self.pending_cr = fixed.ends_with('\r');
        self.at_line_start = fixed.ends_with('\n');
        self.event(EventKind::Output, &fixed)
    }

    /// Start a new line unless the recording is already at one.
    pub fn ensure_line_start(&mut self) -> Result<()> {
        if self.at_line_start {
            return Ok(());
        }
        self.text("\n")
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.event(EventKind::Resize, &format!("{cols}x{rows}"))
    }
}

pub struct Event {
    pub time: f64,
    pub kind: String,
    pub data: String,
}

/// Parse a .cast file's events (skipping the header line). Tolerates a
/// truncated final line (a session killed mid-write must still be auditable).
pub fn read(path: &Path) -> Result<Vec<Event>> {
    let file = fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut lines = BufReader::new(file).lines();
    let header_line = lines
        .next()
        .context("recording is empty")??;
    serde_json::from_str::<serde_json::Value>(&header_line).context("parsing asciicast header")?;
    let mut events = Vec::new();
    for line in lines {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok((time, kind, data)) = serde_json::from_str::<(f64, String, String)>(&line) else {
            continue; // truncated tail
        };
        events.push(Event { time, kind, data });
    }
    Ok(events)
}

/// Render the recording's output stream as plain text: strips ANSI escape
/// sequences for a readable transcript.
pub fn render_plain_text(events: &[Event]) -> String {
    let mut raw = String::new();
    for e in events {
        match e.kind.as_str() {
            "o" => raw.push_str(&e.data),
            "m" => raw.push_str(&format!("\n--- [{:.1}s] {} ---\n", e.time, e.data)),
            _ => {}
        }
    }
    strip_ansi(&raw)
}

/// Minimal ANSI/control stripper good enough for transcripts: removes CSI, OSC,
/// and other ESC sequences, keeps \n and \t, drops other control chars (\r kept
/// as newline collapse).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    // CSI: params then a final byte in @..~
                    for c2 in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c2) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    // OSC: terminated by BEL or ESC \
                    while let Some(c2) = chars.next() {
                        if c2 == '\u{07}' {
                            break;
                        }
                        if c2 == '\u{1b}' {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            },
            '\r' => {}
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}
