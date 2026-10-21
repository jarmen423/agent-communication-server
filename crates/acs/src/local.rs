//! Private local state. Tokens and complete credentials are atomically replaced.
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize, Clone)]
pub struct Agent {
    pub role: String,
    pub public_key: String,
    pub expires_at: u64,
}

#[derive(Serialize, Deserialize)]
pub struct State {
    pub server: String,
    pub tenant: String,
    pub device: String,
    pub nats_url: String,
    pub access_token: String,
    pub access_expires_at: u64,
    pub agents: BTreeMap<String, Agent>,
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is unset")
}

pub fn root() -> Result<PathBuf> {
    let root = std::env::var_os("ACS_HOME")
        .map(PathBuf::from)
        .unwrap_or(home()?.join(".config/acs"));
    fs::create_dir_all(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    }
    Ok(root.canonicalize()?)
}

pub fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("file must have a parent")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".acs-{}-{:016x}.tmp",
        std::process::id(),
        rand::random::<u64>()
    ));
    let result = (|| {
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub fn lock(root: &Path, name: &str) -> Result<File> {
    let mut opts = OpenOptions::new();
    opts.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts.open(root.join(name))?;
    file.try_lock_exclusive()
        .context("another acs operation is running; retry shortly")?;
    Ok(file)
}

pub fn load(root: &Path) -> Result<State> {
    let bytes = fs::read(root.join("state.json")).context("not logged in; run acs login")?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn save(root: &Path, state: &State) -> Result<()> {
    atomic(&root.join("state.json"), &serde_json::to_vec_pretty(state)?)
}

fn entry(root: &Path) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new("acs-device", &root.to_string_lossy())?)
}

pub fn store_refresh(root: &Path, token: &str) -> Result<()> {
    // Write the crash-safe fallback first; remove it only after keychain success.
    atomic(&root.join("refresh.token"), token.as_bytes())?;
    if std::env::var_os("ACS_NO_KEYRING").is_none() {
        if let Ok(entry) = entry(root) {
            if entry.set_password(token).is_ok() {
                atomic(&root.join("keychain.used"), b"keychain")?;
                fs::remove_file(root.join("refresh.token"))?;
                return Ok(());
            }
        }
        eprintln!("OS keychain unavailable; using private 0600 refresh-token file");
    }
    Ok(())
}

pub fn refresh(root: &Path) -> Result<String> {
    // A file wins if a previous keychain write failed during rotation.
    if let Ok(token) = fs::read_to_string(root.join("refresh.token")) {
        return Ok(token);
    }
    entry(root)?
        .get_password()
        .context("device refresh token unavailable; log in again")
}

pub fn clear(root: &Path) -> Result<()> {
    if root.join("keychain.used").exists() {
        let entry = entry(root)?;
        {
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => (),
                Err(_) => {
                    bail!("cannot remove keychain credential; unlock the keychain and retry logout")
                }
            }
        }
    }
    for name in [
        "refresh.token",
        "state.json",
        "keychain.used",
        "logout.revoked",
    ] {
        let path = root.join(name);
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    let creds = root.join("creds");
    if creds.exists() {
        fs::remove_dir_all(creds)?;
    }
    Ok(())
}

pub fn creds_path(root: &Path, id: &str) -> PathBuf {
    root.join("creds").join(format!("{id}.creds"))
}

pub fn write_creds(root: &Path, id: &str, jwt: &str, seed: &str) -> Result<()> {
    atomic(&creds_path(root, id), format!("-----BEGIN NATS USER JWT-----\n{jwt}\n------END NATS USER JWT------\n\n-----BEGIN USER NKEY SEED-----\n{seed}\n------END USER NKEY SEED------\n").as_bytes())
}

pub fn seed(root: &Path, id: &str) -> Result<String> {
    let text = fs::read_to_string(creds_path(root, id))?;
    text.lines()
        .skip_while(|l| !l.contains("BEGIN USER NKEY SEED"))
        .nth(1)
        .map(str::to_owned)
        .context("invalid local credential file")
}
