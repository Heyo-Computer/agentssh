mod cli;
mod config;
mod recording;
mod remote;
mod session;
mod ssh;
mod store;
mod web;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Command, ContextCmd, SessionsCmd, ShellCmd};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match run(cli).await {
        // Interactive/run paths return the remote exit code; exit with it so
        // agents can rely on `agentssh run` behaving like ssh.
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("agentssh: error: {e:#}");
            std::process::exit(255);
        }
    }
}

async fn run(cli: Cli) -> Result<i32> {
    match cli.command {
        Command::Context(cmd) => {
            context_cmd(cmd)?;
            Ok(0)
        }
        Command::Run { context, timeout, command } => {
            session::run::run(&context, &command, timeout).await
        }
        Command::Exec { context, timeout, fresh, command } => {
            session::shell::exec(&context, &command, timeout, fresh).await
        }
        Command::Shell(cmd) => match cmd {
            ShellCmd::Start { context } => session::shell::start(&context).await,
            ShellCmd::List { all } => session::shell::list(all),
            ShellCmd::Interrupt { target } => session::shell::interrupt(&target).await,
            ShellCmd::Stop { target, all } => session::shell::stop(target.as_deref(), all).await,
        },
        Command::Connect { context, no_record_input } => {
            session::interactive::connect(&context, !no_record_input).await
        }
        Command::Attach { session, no_record_input } => {
            session::interactive::attach(&session, !no_record_input).await
        }
        Command::Sessions(cmd) => {
            match cmd {
                SessionsCmd::List { context, active, limit } => {
                    session::sessions::list(context.as_deref(), active, limit)?
                }
                SessionsCmd::Show { session } => session::sessions::show(&session, 2000)?,
                SessionsCmd::Export { session, format, output } => {
                    session::sessions::export(&session, format, output.as_deref())?
                }
                SessionsCmd::Rm { session } => session::sessions::rm(&session)?,
            }
            Ok(0)
        }
        Command::Web { bind, port, allow_remote } => {
            web::serve(&bind, port, allow_remote).await?;
            Ok(0)
        }
    }
}

fn context_cmd(cmd: ContextCmd) -> Result<()> {
    match cmd {
        ContextCmd::Add { name, host, port, user, key, agent, host_key_policy } => {
            let auth = match (key, agent) {
                (Some(path), false) => config::Auth::Key { path },
                (None, true) => config::Auth::Agent,
                (None, false) => anyhow::bail!("specify --key <path> or --agent"),
                (Some(_), true) => unreachable!("clap conflicts_with"),
            };
            let policy = match host_key_policy {
                cli::HostKeyPolicyArg::Strict => config::HostKeyPolicy::Strict,
                cli::HostKeyPolicyArg::AcceptNew => config::HostKeyPolicy::AcceptNew,
            };
            let mut file = config::load_contexts()?;
            let replaced = file
                .contexts
                .insert(name.clone(), config::SshContext { host, port, user, auth, host_key_policy: policy })
                .is_some();
            config::save_contexts(&file)?;
            println!("{} context '{name}'", if replaced { "updated" } else { "added" });
        }
        ContextCmd::List => {
            let file = config::load_contexts()?;
            if file.contexts.is_empty() {
                println!("no contexts (add one with: agentssh context add ...)");
                return Ok(());
            }
            println!("{:<16} {:<24} {:<12} {:<8} POLICY", "NAME", "HOST", "USER", "AUTH");
            for (name, c) in &file.contexts {
                let auth = match &c.auth {
                    config::Auth::Key { .. } => "key",
                    config::Auth::Agent => "agent",
                };
                let policy = match c.host_key_policy {
                    config::HostKeyPolicy::Strict => "strict",
                    config::HostKeyPolicy::AcceptNew => "accept-new",
                };
                println!("{:<16} {:<24} {:<12} {:<8} {policy}", name, format!("{}:{}", c.host, c.port), c.user, auth);
            }
        }
        ContextCmd::Show { name } => {
            let file = config::load_contexts()?;
            let c = file
                .contexts
                .get(&name)
                .ok_or_else(|| anyhow::anyhow!("no context named '{name}'"))?;
            println!("name            {name}");
            println!("host            {}:{}", c.host, c.port);
            println!("user            {}", c.user);
            match &c.auth {
                config::Auth::Key { path } => println!("auth            key file at {path}"),
                config::Auth::Agent => println!("auth            ssh-agent"),
            }
            println!(
                "host key policy {}",
                match c.host_key_policy {
                    config::HostKeyPolicy::Strict => "strict",
                    config::HostKeyPolicy::AcceptNew => "accept-new",
                }
            );
        }
        ContextCmd::Remove { name } => {
            let mut file = config::load_contexts()?;
            if file.contexts.remove(&name).is_none() {
                anyhow::bail!("no context named '{name}'");
            }
            config::save_contexts(&file)?;
            println!("removed context '{name}'");
        }
    }
    Ok(())
}
