//! Identity manifest wire record: who owns an identity, recorded by whoever
//! provisions credentials (a provisioning system or `hub-admin`).
//!
//! The hub keeps one record per identity, uses it to stamp message provenance
//! (`meta.origin`) and stores the identity's accept policy. Unknown fields are
//! ignored, so a provisioning system can add fields before a hub knows them.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::protocol::{reserved_identity, valid_identity};
use crate::query_api::OP_NAMESPACES;

/// Longest identity or user handle in a record, in bytes.
pub const MAX_IDENTITY_BYTES: usize = 64;
/// Longest `device` label, in bytes.
pub const MAX_DEVICE_BYTES: usize = 128;
/// Longest `agent` label, in bytes.
pub const MAX_AGENT_BYTES: usize = 64;
/// Most entries in an `accept.users` list.
pub const MAX_ACCEPT_USERS: usize = 256;
/// Most records in one full sync.
pub const MAX_SYNC_RECORDS: usize = 2000;

/// Identity the hub's owner uses. It has a record and accepts tasks from the
/// whole tenant by default.
const OWNER_IDENTITY: &str = "owner";

/// Who owns an identity: a person, or the tenant itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerKind {
    User,
    Tenant,
}

impl OwnerKind {
    /// The wire name.
    pub fn as_str(&self) -> &'static str {
        match self {
            OwnerKind::User => "user",
            OwnerKind::Tenant => "tenant",
        }
    }
}

/// How an identity connects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityKind {
    Device,
    Connector,
}

impl IdentityKind {
    /// The default for a record that doesn't say.
    pub fn device() -> Self {
        IdentityKind::Device
    }
}

/// What an identity is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Worker,
    Orchestrator,
    Admin,
    Service,
}

impl Role {
    /// The default for a record that doesn't say.
    pub fn worker() -> Self {
        Role::Worker
    }
}

/// Who may give an identity tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum AcceptPolicy {
    /// Identities of the same owning user.
    Owner,
    /// Identities owned by the listed users.
    Users { users: Vec<String> },
    /// Any identity in the tenant.
    Tenant,
}

/// One identity's manifest record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestRecord {
    pub identity: String,
    pub owner_kind: OwnerKind,
    /// Handle of the owning user; null when the tenant owns the identity.
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default = "IdentityKind::device")]
    pub kind: IdentityKind,
    /// Agent client, e.g. `claude-code` or `claude.ai`.
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default = "Role::worker")]
    pub role: Role,
    /// Missing means the default in [`ManifestRecord::effective_accept`].
    #[serde(default)]
    pub accept: Option<AcceptPolicy>,
    /// Comes from one increasing sequence, comparable across identities and
    /// with gaps. A write at or below the stored rev changes nothing.
    pub rev: i64,
}

impl ManifestRecord {
    /// The accept policy in force. Without an explicit one: `tenant` for
    /// tenant-owned identities, for the `owner` identity and for a person's
    /// own handle (an identity named after its user, with no agent client);
    /// `owner` for any other user-owned identity.
    pub fn effective_accept(&self) -> AcceptPolicy {
        if let Some(accept) = &self.accept {
            return accept.clone();
        }
        let human_handle = self.agent.is_none() && self.user.as_deref() == Some(&self.identity);
        match self.owner_kind {
            OwnerKind::Tenant => AcceptPolicy::Tenant,
            OwnerKind::User if human_handle || self.identity == OWNER_IDENTITY => {
                AcceptPolicy::Tenant
            }
            OwnerKind::User => AcceptPolicy::Owner,
        }
    }

    /// Check every field's rule; the error reads `"<field>: <why>"`.
    pub fn validate(&self) -> Result<(), String> {
        check_identity(&self.identity)?;
        match self.owner_kind {
            OwnerKind::User if self.user.is_none() => {
                return Err("user: required when owner_kind is user".to_string());
            }
            OwnerKind::Tenant if self.user.is_some() => {
                return Err("user: must be null when owner_kind is tenant".to_string());
            }
            OwnerKind::Tenant if self.user_id.is_some() => {
                return Err("user_id: must be null when owner_kind is tenant".to_string());
            }
            _ => {}
        }
        if let Some(user) = &self.user {
            check_handle("user", user)?;
        }
        if let Some(id) = &self.user_id {
            check_uuid("user_id", id)?;
        }
        if let Some(id) = &self.device_id {
            check_uuid("device_id", id)?;
        }
        if let Some(device) = &self.device {
            check_device(device)?;
        }
        if let Some(agent) = &self.agent {
            check_agent(agent)?;
        }
        if let Some(AcceptPolicy::Users { users }) = &self.accept {
            check_accept_users(users)?;
        }
        if self.rev < 0 {
            return Err("rev: must be at least 0".to_string());
        }
        Ok(())
    }
}

/// Rules for a name a manifest record can be about: a valid identity of at
/// most [`MAX_IDENTITY_BYTES`] bytes, not reserved for a hub component and not
/// an API op namespace. The error reads `"identity: <why>"`.
pub fn check_identity(identity: &str) -> Result<(), String> {
    check_handle("identity", identity)?;
    if reserved_identity(identity) {
        return Err("identity: reserved for a hub component".to_string());
    }
    if OP_NAMESPACES.contains(&identity) {
        return Err("identity: collides with a query-API op namespace".to_string());
    }
    Ok(())
}

fn check_handle(field: &str, value: &str) -> Result<(), String> {
    if !valid_identity(value) {
        return Err(format!("{field}: must be one or more of [A-Za-z0-9_-]"));
    }
    if value.len() > MAX_IDENTITY_BYTES {
        return Err(format!("{field}: longer than {MAX_IDENTITY_BYTES} bytes"));
    }
    Ok(())
}

fn check_uuid(field: &str, value: &str) -> Result<(), String> {
    // `parse_str` also takes the simple, braced and URN forms; only the
    // hyphenated form is exactly 36 bytes.
    if value.len() == 36 && uuid::Uuid::parse_str(value).is_ok() {
        Ok(())
    } else {
        Err(format!("{field}: must be a hyphenated UUID"))
    }
}

fn check_device(device: &str) -> Result<(), String> {
    if device.is_empty() || device.len() > MAX_DEVICE_BYTES {
        return Err(format!("device: must be 1 to {MAX_DEVICE_BYTES} bytes"));
    }
    if device.chars().any(char::is_control) {
        return Err("device: must not contain control characters".to_string());
    }
    Ok(())
}

fn check_agent(agent: &str) -> Result<(), String> {
    if agent.is_empty() || agent.len() > MAX_AGENT_BYTES {
        return Err(format!("agent: must be 1 to {MAX_AGENT_BYTES} bytes"));
    }
    let allowed = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-');
    if !agent.bytes().all(allowed) {
        return Err("agent: must be made of [A-Za-z0-9._-]".to_string());
    }
    Ok(())
}

fn check_accept_users(users: &[String]) -> Result<(), String> {
    if users.is_empty() || users.len() > MAX_ACCEPT_USERS {
        return Err(format!(
            "accept.users: must have 1 to {MAX_ACCEPT_USERS} entries"
        ));
    }
    let mut seen = HashSet::new();
    for user in users {
        check_handle("accept.users", user)?;
        if !seen.insert(user) {
            return Err(format!("accept.users: duplicate entry {user}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
