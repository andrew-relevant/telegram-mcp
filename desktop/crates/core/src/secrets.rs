//! Credential records kept outside the SQLite session file.
//!
//! `api_id`, `api_hash`, and each datacenter `auth_key` are separate records.
//! An auth key is 256 raw bytes, under the Windows Credential Manager limit.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

/// Windows credential service. macOS builds do not open it.
pub const SERVICE_NAME: &str = "app.telegram-mcp.desktop";
pub const API_ID_ACCOUNT: &str = "api-id";
pub const API_HASH_ACCOUNT: &str = "api-hash";
const AUTH_KEY_INDEX_ACCOUNT: &str = "auth-key-index";

#[derive(Debug)]
pub struct SecretStoreError(String);

impl SecretStoreError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for SecretStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SecretStoreError {}

pub trait SecretStore: Send + Sync + 'static {
    fn get(&self, account: &str) -> Result<Option<Vec<u8>>, SecretStoreError>;
    fn set(&self, account: &str, secret: &[u8]) -> Result<(), SecretStoreError>;
    fn delete(&self, account: &str) -> Result<(), SecretStoreError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiCredentials {
    pub api_id: i32,
    pub api_hash: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ApiCredentialChange {
    Created,
    Updated,
    Unchanged,
    /// The stored application id differs. The caller must log out and wipe
    /// the session before saving the new credentials.
    ApiIdChanged {
        previous: i32,
    },
}

pub fn load_api_credentials(
    store: &impl SecretStore,
) -> Result<Option<ApiCredentials>, SecretStoreError> {
    let Some(api_id_bytes) = store.get(API_ID_ACCOUNT)? else {
        return Ok(None);
    };
    let Some(api_hash_bytes) = store.get(API_HASH_ACCOUNT)? else {
        return Ok(None);
    };
    let api_id = std::str::from_utf8(&api_id_bytes)
        .ok()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| SecretStoreError::new("запись api_id повреждена"))?;
    let api_hash = String::from_utf8(api_hash_bytes)
        .map_err(|_| SecretStoreError::new("запись api_hash повреждена"))?;
    Ok(Some(ApiCredentials { api_id, api_hash }))
}

/// Saves credentials when the application id is new or unchanged.
/// A different application id is refused until the session is wiped.
pub fn store_api_credentials(
    store: &impl SecretStore,
    api_id: i32,
    api_hash: &str,
) -> Result<ApiCredentialChange, SecretStoreError> {
    let created = if let Some(existing) = load_api_credentials(store)? {
        if existing.api_id != api_id {
            return Ok(ApiCredentialChange::ApiIdChanged {
                previous: existing.api_id,
            });
        }
        if existing.api_hash == api_hash {
            return Ok(ApiCredentialChange::Unchanged);
        }
        false
    } else {
        true
    };
    store.set(API_ID_ACCOUNT, api_id.to_string().as_bytes())?;
    store.set(API_HASH_ACCOUNT, api_hash.as_bytes())?;
    Ok(if created {
        ApiCredentialChange::Created
    } else {
        ApiCredentialChange::Updated
    })
}

pub fn put_auth_key(
    store: &impl SecretStore,
    dc_id: i32,
    key: &[u8; 256],
) -> Result<(), SecretStoreError> {
    store.set(&auth_key_account(dc_id), key)?;
    let mut ids = auth_key_ids(store)?;
    ids.insert(dc_id);
    write_auth_key_ids(store, &ids)
}

pub fn remove_auth_key(store: &impl SecretStore, dc_id: i32) -> Result<(), SecretStoreError> {
    store.delete(&auth_key_account(dc_id))?;
    let mut ids = auth_key_ids(store)?;
    ids.remove(&dc_id);
    write_auth_key_ids(store, &ids)
}

pub fn load_auth_keys(
    store: &impl SecretStore,
) -> Result<HashMap<i32, [u8; 256]>, SecretStoreError> {
    let mut keys = HashMap::new();
    for dc_id in auth_key_ids(store)? {
        let Some(secret) = store.get(&auth_key_account(dc_id))? else {
            return Err(SecretStoreError::new(format!(
                "нет записи ключа для дата-центра {dc_id}"
            )));
        };
        let key: [u8; 256] = secret.try_into().map_err(|secret: Vec<u8>| {
            SecretStoreError::new(format!(
                "ключ дата-центра {dc_id} имеет длину {}, ожидалась 256",
                secret.len()
            ))
        })?;
        keys.insert(dc_id, key);
    }
    Ok(keys)
}

pub fn wipe_secrets(store: &impl SecretStore) -> Result<(), SecretStoreError> {
    let ids = auth_key_ids(store)?;
    for dc_id in ids {
        store.delete(&auth_key_account(dc_id))?;
    }
    store.delete(AUTH_KEY_INDEX_ACCOUNT)?;
    store.delete(API_ID_ACCOUNT)?;
    store.delete(API_HASH_ACCOUNT)?;
    Ok(())
}

fn auth_key_account(dc_id: i32) -> String {
    format!("auth-key-{dc_id}")
}

fn auth_key_ids(store: &impl SecretStore) -> Result<BTreeSet<i32>, SecretStoreError> {
    let Some(bytes) = store.get(AUTH_KEY_INDEX_ACCOUNT)? else {
        return Ok(BTreeSet::new());
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| SecretStoreError::new("список ключей дата-центров поврежден"))?;
    let mut ids = BTreeSet::new();
    for piece in text.split(',').filter(|piece| !piece.is_empty()) {
        let id = piece
            .parse()
            .map_err(|_| SecretStoreError::new("список ключей дата-центров поврежден"))?;
        ids.insert(id);
    }
    Ok(ids)
}

fn write_auth_key_ids(
    store: &impl SecretStore,
    ids: &BTreeSet<i32>,
) -> Result<(), SecretStoreError> {
    if ids.is_empty() {
        return store.delete(AUTH_KEY_INDEX_ACCOUNT);
    }
    let text = ids.iter().map(i32::to_string).collect::<Vec<_>>().join(",");
    store.set(AUTH_KEY_INDEX_ACCOUNT, text.as_bytes())
}

/// In-memory store used by tests. It never touches the system keychain.
#[derive(Clone, Default)]
pub struct MemorySecretStore {
    entries: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

impl SecretStore for MemorySecretStore {
    fn get(&self, account: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        let entries = self
            .entries
            .lock()
            .map_err(|_| SecretStoreError::new("хранилище секретов повреждено"))?;
        Ok(entries.get(account).cloned())
    }

    fn set(&self, account: &str, secret: &[u8]) -> Result<(), SecretStoreError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| SecretStoreError::new("хранилище секретов повреждено"))?;
        entries.insert(account.to_string(), secret.to_vec());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), SecretStoreError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| SecretStoreError::new("хранилище секретов повреждено"))?;
        entries.remove(account);
        Ok(())
    }
}

/// Owner-only files on macOS, Windows Credential Manager elsewhere.
/// The stdio shim must not construct this.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemSecretStore;

#[cfg(target_os = "macos")]
impl SecretStore for SystemSecretStore {
    fn get(&self, account: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        crate::secret_file::get(&secret_root()?, account)
    }

    fn set(&self, account: &str, secret: &[u8]) -> Result<(), SecretStoreError> {
        crate::secret_file::set(&secret_root()?, account, secret)
    }

    fn delete(&self, account: &str) -> Result<(), SecretStoreError> {
        crate::secret_file::delete(&secret_root()?, account)
    }
}

#[cfg(target_os = "macos")]
fn secret_root() -> Result<std::path::PathBuf, SecretStoreError> {
    telegram_mcp_ipc::app_data_dir()
        .map_err(|_| SecretStoreError::new("не найден каталог приложения"))
}

#[cfg(not(target_os = "macos"))]
impl SecretStore for SystemSecretStore {
    fn get(&self, account: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        match keyring_entry(account)?.get_secret() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(secret_error(error)),
        }
    }

    fn set(&self, account: &str, secret: &[u8]) -> Result<(), SecretStoreError> {
        keyring_entry(account)?
            .set_secret(secret)
            .map_err(secret_error)
    }

    fn delete(&self, account: &str) -> Result<(), SecretStoreError> {
        match keyring_entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(secret_error(error)),
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn keyring_entry(account: &str) -> Result<keyring::Entry, SecretStoreError> {
    keyring::Entry::new(SERVICE_NAME, account).map_err(secret_error)
}

#[cfg(not(target_os = "macos"))]
fn secret_error(error: keyring::Error) -> SecretStoreError {
    SecretStoreError::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_id_change_does_not_replace_credentials() {
        let store = MemorySecretStore::default();
        assert_eq!(
            store_api_credentials(&store, 1, "hash-one").unwrap(),
            ApiCredentialChange::Created
        );
        assert_eq!(
            store_api_credentials(&store, 2, "hash-two").unwrap(),
            ApiCredentialChange::ApiIdChanged { previous: 1 }
        );
        let saved = load_api_credentials(&store).unwrap().unwrap();
        assert_eq!(saved.api_id, 1);
        assert_eq!(saved.api_hash, "hash-one");
    }

    #[test]
    fn wipe_removes_credentials_and_auth_keys() {
        let store = MemorySecretStore::default();
        store_api_credentials(&store, 7, "hash-seven").unwrap();
        put_auth_key(&store, 2, &[0x11; 256]).unwrap();
        put_auth_key(&store, 4, &[0x22; 256]).unwrap();
        wipe_secrets(&store).unwrap();
        assert!(load_api_credentials(&store).unwrap().is_none());
        assert!(load_auth_keys(&store).unwrap().is_empty());
    }
}
