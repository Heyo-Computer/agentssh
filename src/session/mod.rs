pub mod interactive;
pub mod reconnect;
pub mod run;
pub mod sessions;
pub mod shell;

/// Terminal geometry to stamp on a recording that has no PTY behind it.
///
/// The remote never wrapped this output, so the number decides only where the
/// *player* wraps it on replay. The local terminal size is a poor answer and an
/// actively bad one under an agent, where there is no tty at all and the size
/// falls back to 80 columns — narrow enough to fold almost any real command
/// output into an unreadable zigzag.
pub fn record_size() -> (u16, u16) {
    let (cols, rows) = interactive::term_size();
    (cols.max(120), rows.max(30))
}
