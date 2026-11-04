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
    #[serde(default)]
    pub device_name: Option<String>,
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

/// RFC 3339 UTC for Unix seconds (days-to-civil, H. Hinnant; no time crate).
pub fn rfc3339(secs: u64) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

/// Coarse duration: 45s, 59m, 23h, 3d.
fn span(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 2 * 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

/// Credential expiry as `expires <RFC 3339> (in 23h)` or `expired … (3h ago)`.
pub fn expiry(at: u64) -> String {
    let now = now();
    match at {
        0 => "not issued yet".into(),
        at if at > now => format!("expires {} (in {})", rfc3339(at), span(at - now)),
        at => format!("expired {} ({} ago)", rfc3339(at), span(now - at)),
    }
}

pub fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is unset")
}

pub fn root() -> Result<PathBuf> {
    let root = match std::env::var_os("ACS_HOME") {
        Some(path) => PathBuf::from(path),
        None => home()?.join(".config/acs"),
    };
    fs::create_dir_all(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    }
    Ok(root.canonicalize()?)
}

pub fn private_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            private_dir(parent)?;
        }
        fs::create_dir(path)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("file must have a parent")?;
    private_dir(parent)?;
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
    crate::tenants::load(root)
}

pub fn save(root: &Path, state: &State) -> Result<()> {
    crate::tenants::save(root, state)
}

fn entry(root: &Path) -> Result<keyring::Entry> {
    let root = crate::tenants::device_root(root);
    Ok(keyring::Entry::new("acs-device", &root.to_string_lossy())?)
}

pub fn store_refresh(root: &Path, token: &str) -> Result<()> {
    let root = crate::tenants::device_root(root);
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
    let root = crate::tenants::device_root(root);
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
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => (),
            Err(_) => {
                bail!("cannot remove keychain credential; unlock the keychain and retry logout")
            }
        }
    }
    let creds = root.join("creds");
    if creds.is_symlink() {
        fs::remove_file(&creds)?;
    }
    if root.join("tenants").exists() {
        fs::remove_dir_all(root.join("tenants"))?;
    }
    if creds.exists() {
        fs::remove_dir_all(creds)?;
    }
    for name in [
        "refresh.token",
        "keychain.used",
        "state.json",
        "state.json.bak",
        "device.json",
        "current",
        "logout.revoked",
    ] {
        let path = root.join(name);
        if path.exists() {
            fs::remove_file(path)?;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_expiry_as_rfc3339_and_relative_time() {
        for (secs, text) in [
            (0, "1970-01-01T00:00:00Z"),
            (951_782_400, "2000-02-29T00:00:00Z"),
            (1_709_164_800, "2024-02-29T00:00:00Z"),
            (1_793_750_400, "2026-11-04T00:00:00Z"),
            (4_102_444_799, "2099-12-31T23:59:59Z"),
        ] {
            assert_eq!(rfc3339(secs), text);
        }
        assert_eq!(span(45), "45s");
        assert_eq!(span(59 * 60 + 59), "59m");
        assert_eq!(span(23 * 3_600 + 59 * 60), "23h");
        assert_eq!(span(3 * 86_400), "3d");
        let soon = expiry(now() + 23 * 3_600 + 30);
        assert!(
            soon.starts_with("expires 20") && soon.ends_with("(in 23h)"),
            "{soon}"
        );
        assert!(expiry(now() - 120).ends_with("(2m ago)"));
        assert_eq!(expiry(0), "not issued yet");
    }
}
