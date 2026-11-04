//! One identity's NATS environment: `acs env`, `acs exec` and the process
//! handoff shared with `acs worker run`.
use crate::{
    agents,
    local::{self, State},
};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Auth inherited from the caller would compete with the identity's
/// credentials (nats-py sends a stray token or user alongside them).
/// The device service key is never handed to a child.
const STRIPPED_NATS: &[&str] = &[
    "NATS_USER",
    "NATS_PASSWORD",
    "NATS_TOKEN",
    "NATS_NKEY",
    "NATS_NKEYS_SEED",
];

/// The four variables clients read: tenant, URL, credential and identity.
pub fn vars(root: &Path, state: &State, id: &str) -> [(&'static str, String); 4] {
    [
        ("ACS_TENANT", state.tenant.clone()),
        ("NATS_URL", state.nats_url.clone()),
        (
            "NATS_CREDENTIALS_FILE",
            local::creds_path(root, id).to_string_lossy().into_owned(),
        ),
        ("NATS_HUB_IDENTITY", id.to_owned()),
    ]
}

pub fn apply(cmd: &mut tokio::process::Command, root: &Path, state: &State, id: &str) {
    for (key, value) in vars(root, state, id) {
        cmd.env(key, value);
    }
    for key in STRIPPED_NATS {
        cmd.env_remove(key);
    }
    cmd.env_remove("ACS_SERVICE_KEY");
}

/// Load the state and check the identity; warn when its credential lapsed.
fn load(root: &Path, identity: &str) -> Result<State> {
    let state = local::load(root)?;
    let id = agents::select(&state, Some(identity))?;
    let expires = state.agents[id].expires_at;
    if expires <= local::now() {
        eprintln!(
            "warning: {id} credential {}; start the daemon with `acs up`",
            local::expiry(expires)
        );
    }
    Ok(state)
}

/// POSIX single quoting: everything is literal except `'`, written `'\''`.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// `acs env <id>`: lines for `eval "$(acs env <id>)"`.
pub fn print(root: &Path, identity: &str) -> Result<()> {
    let state = load(root, identity)?;
    println!("unset {}", STRIPPED_NATS.join(" "));
    for (key, value) in vars(root, &state, identity) {
        println!("export {key}={}", quote(&value));
    }
    Ok(())
}

/// A bare command name resolves through PATH, then beside `acs` itself, so
/// `acs exec id -- hub-delegate …` works before the install dir is on PATH.
fn program(name: &str) -> PathBuf {
    let path = Path::new(name);
    if path.components().count() != 1 {
        return path.into();
    }
    let on_path = std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|dir| dir.join(name).is_file()));
    if !on_path {
        if let Some(sibling) = std::env::current_exe()
            .ok()
            .map(|exe| exe.with_file_name(name))
            .filter(|p| p.is_file())
        {
            return sibling;
        }
    }
    path.into()
}

/// `acs exec <id> -- <cmd> [args…]`.
pub async fn run(root: &Path, identity: &str, command: &[String]) -> Result<()> {
    let state = load(root, identity)?;
    let (name, args) = command.split_first().context("no command given after --")?;
    let mut cmd = tokio::process::Command::new(program(name));
    cmd.args(args);
    apply(&mut cmd, root, &state, identity);
    handoff(cmd)
        .await
        .with_context(|| format!("cannot run {name}"))
}

/// Replace this process (Unix) so signals and the exit status are the child's.
pub async fn handoff(mut cmd: tokio::process::Command) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(cmd.as_std_mut().exec().into())
    }
    #[cfg(not(unix))]
    {
        let status = cmd.status().await?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_for_posix_eval() {
        assert_eq!(quote("plain"), "'plain'");
        assert_eq!(quote("it's $HOME `x`"), r"'it'\''s $HOME `x`'");
    }
}
