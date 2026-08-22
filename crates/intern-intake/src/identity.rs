//! Stable per-machine identity for claims, origins, and presence.

use std::{
    fs, io,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};

use crate::fsatomic;

static ID_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MachineIdentity {
    pub id: String,
    pub name: String,
    pub user: String,
}

impl MachineIdentity {
    /// Loads or creates the `machine-id` file in `data_dir` (32 lowercase hex
    /// characters derived from sha256 of hostname|user|pid|nanos|counter).
    ///
    /// The id must survive renames of the machine or user, so it is random at
    /// birth and durable afterwards; `name` and `user` are cosmetic and
    /// re-resolved on every load. A non-blank `label` overrides the hostname
    /// as the display name.
    pub fn load_or_create(data_dir: &Path, label: &str) -> io::Result<MachineIdentity> {
        fs::create_dir_all(data_dir)?;
        let path = data_dir.join("machine-id");
        let id = match fs::read_to_string(&path) {
            Ok(text) => match parse_id(&text) {
                Some(id) => id,
                None => {
                    let id = generate_id();
                    fsatomic::replace_file(&path, id.as_bytes())?;
                    id
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let id = generate_id();
                match fsatomic::create_exclusive(&path, id.as_bytes()) {
                    Ok(()) => id,
                    Err(create_error) if create_error.kind() == io::ErrorKind::AlreadyExists => {
                        fs::read_to_string(&path)
                            .ok()
                            .as_deref()
                            .and_then(parse_id)
                            .unwrap_or(id)
                    }
                    Err(create_error) => return Err(create_error),
                }
            }
            Err(error) => return Err(error),
        };
        Ok(Self {
            id,
            name: resolve_name(label),
            user: resolve_user(),
        })
    }
}

fn parse_id(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.len() == 32 && trimmed.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Some(trimmed.to_ascii_lowercase())
    } else {
        None
    }
}

fn generate_id() -> String {
    let sequence = ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seed = format!(
        "{}|{}|{}|{nanos}|{sequence}",
        hostname(),
        resolve_user(),
        std::process::id()
    );
    let mut hasher = Sha256::new();
    hasher.update(seed.as_bytes());
    format!("{:x}", hasher.finalize())[..32].to_string()
}

fn resolve_name(label: &str) -> String {
    let label = label.trim();
    if label.is_empty() {
        hostname()
    } else {
        label.to_string()
    }
}

fn hostname() -> String {
    for variable in ["COMPUTERNAME", "HOSTNAME"] {
        if let Some(value) = nonblank_var(variable) {
            return value;
        }
    }
    #[cfg(unix)]
    if let Ok(text) = fs::read_to_string("/etc/hostname") {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "this-machine".to_string()
}

fn resolve_user() -> String {
    for variable in ["USERNAME", "USER"] {
        if let Some(value) = nonblank_var(variable) {
            return value;
        }
    }
    "unknown".to_string()
}

fn nonblank_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
