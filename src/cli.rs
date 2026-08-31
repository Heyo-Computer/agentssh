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
    /// Run a single command on a context's remote host
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
