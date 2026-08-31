use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshContext {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    pub auth: Auth,
    #[serde(default)]
    pub host_key_policy: HostKeyPolicy,
}

fn default_port() -> u16 {
    22
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Auth {
    /// Private key file on disk; passphrase (if any) is prompted, never stored.
    Key { path: String },
    /// Delegate to the running ssh-agent via SSH_AUTH_SOCK.
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostKeyPolicy {
    Strict,
    #[default]
    AcceptNew,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ContextsFile {
    #[serde(default)]
    pub contexts: BTreeMap<String, SshContext>,
}

pub struct Paths {
    pub contexts_file: PathBuf,
    pub db_file: PathBuf,
    pub recordings_dir: PathBuf,
}

pub fn paths() -> Result<Paths> {
    let dirs = directories::ProjectDirs::from("", "", "agentssh")
        .context("could not determine home directory")?;
    Ok(Paths {
        contexts_file: dirs.config_dir().join("contexts.toml"),
        db_file: dirs.data_dir().join("agentssh.db"),
        recordings_dir: dirs.data_dir().join("recordings"),
    })
}

/// Create a directory (and parents) with 0700 permissions.
pub fn ensure_private_dir(dir: &std::path::Path) -> Result<()> {
    if !dir.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("creating {}", dir.display()))?;
    }
    Ok(())
}

pub fn load_contexts() -> Result<ContextsFile> {
    let path = paths()?.contexts_file;
    if !path.exists() {
        return Ok(ContextsFile::default());
    }
    let mode = fs::metadata(&path)?.permissions().mode();
    if mode & 0o077 != 0 {
        bail!(
            "{} is readable by other users (mode {:o}); it holds connection config.\n\
             Fix it with: chmod 600 {}",
            path.display(),
            mode & 0o777,
            path.display()
        );
    }
    let raw = fs::read_to_string(&path)?;
    toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

pub fn save_contexts(file: &ContextsFile) -> Result<()> {
    let path = paths()?.contexts_file;
    ensure_private_dir(path.parent().unwrap())?;
    let raw = toml::to_string_pretty(file)?;
    let tmp = path.with_extension("toml.tmp");
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(raw.as_bytes())?;
    f.sync_all()?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn get_context(name: &str) -> Result<SshContext> {
    let file = load_contexts()?;
    file.contexts.get(name).cloned().with_context(|| {
        format!(
            "no context named '{name}'. Add one with: agentssh context add {name} --host <h> --user <u> --key <path>"
        )
    })
}
