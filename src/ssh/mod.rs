pub mod exec;
pub mod handler;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use russh::client;
use russh::keys::agent::client::AgentClient;
use russh::keys::key::PrivateKeyWithHashAlg;
use russh::keys::{load_secret_key, ssh_key};

use crate::config::{Auth, SshContext};
use handler::ClientHandler;

pub struct Ssh {
    pub handle: client::Handle<ClientHandler>,
}

/// Connect and authenticate using a context. Credentials never leave this
/// function's scope: callers get an authenticated handle only.
pub async fn connect(ctx: &SshContext) -> Result<Ssh> {
    let config = Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 3,
        nodelay: true,
        ..Default::default()
    });
    let sh = ClientHandler {
        host: ctx.host.clone(),
        port: ctx.port,
        policy: ctx.host_key_policy,
    };
    let mut handle = client::connect(config, (ctx.host.as_str(), ctx.port), sh)
        .await
        .with_context(|| format!("connecting to {}:{}", ctx.host, ctx.port))?;

    match &ctx.auth {
        Auth::Key { path } => {
            let key = load_key(path)?;
            let hash_alg = handle
                .best_supported_rsa_hash()
                .await?
                .flatten();
            let res = handle
                .authenticate_publickey(
                    ctx.user.clone(),
                    PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg),
                )
                .await?;
            if !res.success() {
                bail!(
                    "publickey authentication failed for {}@{} with key {}",
                    ctx.user, ctx.host, path
                );
            }
        }
        Auth::Agent => {
            let mut agent = AgentClient::connect_env()
                .await
                .context("connecting to ssh-agent (is SSH_AUTH_SOCK set?)")?;
            let identities = agent.request_identities().await?;
            if identities.is_empty() {
                bail!("ssh-agent holds no identities (ssh-add a key first)");
            }
            let rsa_hash = handle.best_supported_rsa_hash().await?.flatten();
            let mut authed = false;
            for identity in identities {
                let key: ssh_key::PublicKey = identity.public_key().into_owned();
                let hash_alg = if matches!(key.algorithm(), ssh_key::Algorithm::Rsa { .. }) {
                    rsa_hash
                } else {
                    None
                };
                let res = handle
                    .authenticate_publickey_with(ctx.user.clone(), key, hash_alg, &mut agent)
                    .await?;
                if res.success() {
                    authed = true;
                    break;
                }
            }
            if !authed {
                bail!("ssh-agent authentication failed for {}@{} (no identity accepted)", ctx.user, ctx.host);
            }
        }
    }
    Ok(Ssh { handle })
}

/// Load a private key, prompting for its passphrase if encrypted.
/// The passphrase is never stored and never passed via argv or env.
fn load_key(path: &str) -> Result<ssh_key::PrivateKey> {
    let expanded = shellexpand_home(path);
    match load_secret_key(&expanded, None) {
        Ok(key) => Ok(key),
        Err(russh::keys::Error::KeyIsEncrypted) => {
            let pass = rpassword::prompt_password(format!("Passphrase for {expanded}: "))
                .context("reading passphrase")?;
            load_secret_key(&expanded, Some(&pass))
                .with_context(|| format!("decrypting {expanded} (wrong passphrase?)"))
        }
        Err(e) => Err(e).with_context(|| format!("loading key {expanded}")),
    }
}

fn shellexpand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::home_dir()
    {
        return home.join(rest).to_string_lossy().into_owned();
    }
    path.to_string()
}
