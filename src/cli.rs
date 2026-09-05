use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "agentssh", version, about = "Audited SSH sessions for agents, via named contexts")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Manage remote server contexts
    #[command(subcommand)]
    Context(ContextCmd),
    /// Run a command in the context's persistent remote shell, starting one if
    /// needed. Working directory, environment, and shell state carry over from
    /// the previous `exec` on the same context.
    Exec {
        /// Context name
        context: String,
        /// Abort if the command runs longer than this many seconds (the command
        /// is then interrupted, as Ctrl-C would)
        #[arg(long)]
        timeout: Option<u64>,
        /// End any existing shell for this context and start from a clean one
        #[arg(long)]
        fresh: bool,
        /// Command and arguments to run remotely
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Manage the persistent remote shells that `exec` runs in
    #[command(subcommand)]
    Shell(ShellCmd),
    /// Run a single command on its own connection, with no shell state
    Run {
        /// Context name
        context: String,
        /// Abort if the command runs longer than this many seconds
        #[arg(long)]
        timeout: Option<u64>,
        /// Command and arguments to run remotely
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Open an interactive shell session (inside remote tmux)
    Connect {
        /// Context name
        context: String,
        /// Do not record keystrokes (input events) in the session recording
        #[arg(long)]
        no_record_input: bool,
    },
    /// Reattach a detached or dropped interactive session
    Attach {
        /// Session id (or unique prefix)
        session: String,
        /// Do not record keystrokes (input events) in the session recording
        #[arg(long)]
        no_record_input: bool,
    },
    /// Inspect recorded sessions
    #[command(subcommand)]
    Sessions(SessionsCmd),
    /// Serve the session playback web UI
    Web {
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        #[arg(long, default_value_t = 8787)]
        port: u16,
        /// Required to bind a non-loopback address (the UI has no authentication)
        #[arg(long)]
        allow_remote: bool,
    },
}

#[derive(Subcommand)]
pub enum ShellCmd {
    /// Open a persistent shell for a context without running anything in it
    Start { context: String },
    /// List persistent shells
    List {
        /// Include shells that have already ended
        #[arg(long)]
        all: bool,
    },
    /// Interrupt whatever a shell is currently running (Ctrl-C)
    Interrupt {
        /// Context name, or a session id prefix
        target: String,
    },
    /// End a persistent shell and kill its remote tmux session
    Stop {
        /// Context name, or a session id prefix
        target: Option<String>,
        /// Stop every open shell
        #[arg(long, conflicts_with = "target")]
        all: bool,
    },
}

#[derive(Subcommand)]
pub enum ContextCmd {
    /// Add or replace a context
    Add {
        name: String,
        #[arg(long)]
        host: String,
        #[arg(long, default_value_t = 22)]
        port: u16,
        #[arg(long)]
        user: String,
        /// Path to a private key file
        #[arg(long, conflicts_with = "agent")]
        key: Option<String>,
        /// Authenticate via the running ssh-agent (SSH_AUTH_SOCK)
        #[arg(long)]
        agent: bool,
        #[arg(long, value_enum, default_value_t = HostKeyPolicyArg::AcceptNew)]
        host_key_policy: HostKeyPolicyArg,
    },
    /// List contexts (never prints credentials)
    List,
    /// Show one context (prints the key path, never key contents)
    Show { name: String },
    /// Remove a context
    Remove { name: String },
}

#[derive(Subcommand)]
pub enum SessionsCmd {
    /// List recorded sessions
    List {
        #[arg(long)]
        context: Option<String>,
        /// Only sessions that are active or resumable
        #[arg(long)]
        active: bool,
        #[arg(long, default_value_t = 25)]
        limit: usize,
    },
    /// Show one session's metadata, segments, and output tail
    Show { session: String },
    /// Export a session recording
    Export {
        session: String,
        #[arg(long, value_enum, default_value_t = ExportFormat::Asciicast)]
        format: ExportFormat,
        /// Output file (default: stdout)
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Delete a session and its recording
    Rm { session: String },
}

#[derive(Copy, Clone, ValueEnum)]
pub enum ExportFormat {
    Asciicast,
    Txt,
}

#[derive(Copy, Clone, ValueEnum)]
pub enum HostKeyPolicyArg {
    Strict,
    AcceptNew,
}
