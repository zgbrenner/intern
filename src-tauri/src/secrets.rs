//! Where the hosted model's API key lives.
//!
//! Not in `settings.json`. That file is plain text in the profile, it is
//! what a person pastes into a bug report, and a key in it would be one
//! copy-paste from the wrong inbox. The operating system already keeps a
//! store for exactly this - Windows Credential Manager, the macOS Keychain,
//! the kernel keyring on Linux - where the key is encrypted to the signed-in
//! user and visible to them under Intern's name, to inspect or delete without
//! Intern's help.
//!
//! The store is a seam so tests never touch a real credential manager.

use std::{collections::HashMap, sync::Mutex};

/// The one secret Intern keeps.
pub const HOSTED_MODEL_API_KEY: &str = "hosted-model-api-key";

const SERVICE: &str = "Intern";

pub trait SecretStore: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<String>, String>;
    fn set(&self, name: &str, value: &str) -> Result<(), String>;
    fn delete(&self, name: &str) -> Result<(), String>;
}

/// The operating system's credential store.
#[derive(Clone, Copy, Debug, Default)]
pub struct KeyringStore;

impl KeyringStore {
    fn entry(name: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, name).map_err(|error| error.to_string())
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        match Self::entry(name)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        Self::entry(name)?
            .set_password(value)
            .map_err(|error| error.to_string())
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        match Self::entry(name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// A store that forgets everything when Intern exits, for tests and for a
/// desktop that has no credential store to offer.
#[derive(Debug, Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl SecretStore for MemoryStore {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        Ok(self
            .0
            .lock()
            .map_err(|_| "secret store is unavailable".to_owned())?
            .get(name)
            .cloned())
    }

    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        self.0
            .lock()
            .map_err(|_| "secret store is unavailable".to_owned())?
            .insert(name.to_owned(), value.to_owned());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        self.0
            .lock()
            .map_err(|_| "secret store is unavailable".to_owned())?
            .remove(name);
        Ok(())
    }
}

/// The tail of a key, for Settings to show that one is stored without
/// showing the key: `…a1b2`. Short keys are hidden entirely.
pub fn key_hint(key: &str) -> String {
    let trimmed = key.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() < 12 {
        return "…".to_owned();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::{HOSTED_MODEL_API_KEY, MemoryStore, SecretStore, key_hint};

    #[test]
    fn the_memory_store_round_trips_and_forgets_on_request() {
        let store = MemoryStore::default();
        assert_eq!(store.get(HOSTED_MODEL_API_KEY).unwrap(), None);
        store
            .set(HOSTED_MODEL_API_KEY, "sk-ant-secret-1234")
            .unwrap();
        assert_eq!(
            store.get(HOSTED_MODEL_API_KEY).unwrap().as_deref(),
            Some("sk-ant-secret-1234")
        );
        store.delete(HOSTED_MODEL_API_KEY).unwrap();
        assert_eq!(store.get(HOSTED_MODEL_API_KEY).unwrap(), None);
        store.delete(HOSTED_MODEL_API_KEY).unwrap();
    }

    #[test]
    fn a_hint_shows_the_tail_of_a_key_and_nothing_of_a_short_one() {
        assert_eq!(key_hint("sk-ant-api03-abcdefghijklmnop-Zz19"), "…Zz19");
        assert_eq!(key_hint("  sk-1234567890ab  "), "…90ab");
        assert_eq!(key_hint("short"), "…");
        assert_eq!(key_hint(""), "…");
    }
}
