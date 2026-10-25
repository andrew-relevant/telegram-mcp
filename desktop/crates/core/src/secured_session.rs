//! Session file with datacenter auth keys kept in the secret store.
//!
//! grammers' `SqliteSession` stores `auth_key` in the `dc_option` table.
//! This wrapper saves that column as NULL and keeps each 256-byte key in the
//! secret store. Peer cache and update state stay in the SQLite file.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use grammers_session::storages::{SqliteSession, SqliteSessionError};
use grammers_session::types::{DcOption, PeerId, PeerInfo, UpdateState, UpdatesState};
use grammers_session::{BoxFuture, Session};
use libsql::params;

use crate::secrets::{self, SecretStore, SecretStoreError};

const APP_DIR_NAME: &str = "app.telegram-mcp.desktop";
const SESSION_FILE_NAME: &str = "session.sqlite";

#[derive(Debug)]
pub enum SessionStoreError {
    Sqlite(String),
    Secret(String),
    Poisoned,
    Io(String),
    MissingDataDir,
}

impl std::fmt::Display for SessionStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(message) | Self::Secret(message) | Self::Io(message) => {
                formatter.write_str(message)
            }
            Self::Poisoned => formatter.write_str("сессия повреждена"),
            Self::MissingDataDir => formatter.write_str("каталог данных приложения недоступен"),
        }
    }
}

impl std::error::Error for SessionStoreError {}

impl From<SqliteSessionError> for SessionStoreError {
    fn from(error: SqliteSessionError) -> Self {
        Self::Sqlite(error.to_string())
    }
}

impl From<SecretStoreError> for SessionStoreError {
    fn from(error: SecretStoreError) -> Self {
        Self::Secret(error.to_string())
    }
}

impl From<std::io::Error> for SessionStoreError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

impl From<libsql::Error> for SessionStoreError {
    fn from(error: libsql::Error) -> Self {
        Self::Sqlite(error.to_string())
    }
}

pub struct KeychainSession<S> {
    inner: SqliteSession,
    secrets: S,
    keys: Mutex<HashMap<i32, [u8; 256]>>,
    path: PathBuf,
}

pub fn app_data_dir() -> Result<PathBuf, SessionStoreError> {
    dirs::data_dir()
        .map(|path| path.join(APP_DIR_NAME))
        .ok_or(SessionStoreError::MissingDataDir)
}

pub fn session_file_path() -> Result<PathBuf, SessionStoreError> {
    Ok(app_data_dir()?.join(SESSION_FILE_NAME))
}

pub fn ensure_app_data_dir() -> Result<PathBuf, SessionStoreError> {
    let path = app_data_dir()?;
    fs::create_dir_all(&path)?;
    restrict_owner(&path, true)?;
    Ok(path)
}

impl<S: SecretStore> KeychainSession<S> {
    pub async fn open(path: impl AsRef<Path>, secrets: S) -> Result<Self, SessionStoreError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
                restrict_owner(parent, true)?;
            }
        }
        move_auth_keys_out_of_sqlite(&path, &secrets).await?;
        let inner = SqliteSession::open(&path).await?;
        restrict_session_files(&path)?;
        let keys = secrets::load_auth_keys(&secrets)?;
        Ok(Self {
            inner,
            secrets,
            keys: Mutex::new(keys),
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Deletes secret records and the session file. Call `auth.logOut` first
    /// while the Telegram client is still connected.
    pub async fn wipe(self) -> Result<(), SessionStoreError> {
        let KeychainSession {
            inner,
            secrets,
            path,
            ..
        } = self;
        drop(inner);
        secrets::wipe_secrets(&secrets)?;
        delete_session_files(&path)?;
        Ok(())
    }
}

impl<S: SecretStore> Session for KeychainSession<S> {
    type Error = SessionStoreError;

    fn home_dc_id(&self) -> Result<i32, Self::Error> {
        Ok(self.inner.home_dc_id()?)
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async move {
            self.inner.set_home_dc_id(dc_id).await?;
            restrict_session_files(&self.path)?;
            Ok(())
        })
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, Self::Error> {
        let mut option = self.inner.dc_option(dc_id)?;
        if let Some(option) = option.as_mut() {
            let keys = self.keys.lock().map_err(|_| SessionStoreError::Poisoned)?;
            option.auth_key = keys.get(&dc_id).copied();
        }
        Ok(option)
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, Result<(), Self::Error>> {
        let mut stored = dc_option.clone();
        let key = stored.auth_key.take();
        let dc_id = stored.id;
        Box::pin(async move {
            match key {
                Some(key) => {
                    secrets::put_auth_key(&self.secrets, dc_id, &key)?;
                    self.keys
                        .lock()
                        .map_err(|_| SessionStoreError::Poisoned)?
                        .insert(dc_id, key);
                }
                None => {
                    secrets::remove_auth_key(&self.secrets, dc_id)?;
                    self.keys
                        .lock()
                        .map_err(|_| SessionStoreError::Poisoned)?
                        .remove(&dc_id);
                }
            }
            self.inner.set_dc_option(&stored).await?;
            restrict_session_files(&self.path)?;
            Ok(())
        })
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, Self::Error>> {
        Box::pin(async move { Ok(self.inner.peer(peer).await?) })
    }

    fn cache_peer(&self, peer: PeerInfo) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async move {
            self.inner.cache_peer(peer).await?;
            Ok(())
        })
    }

    fn cache_peers(&self, peers: Vec<PeerInfo>) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async move {
            self.inner.cache_peers(peers).await?;
            Ok(())
        })
    }

    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, Self::Error>> {
        Box::pin(async move { Ok(self.inner.updates_state().await?) })
    }

    fn set_update_state(&self, update: UpdateState) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async move {
            self.inner.set_update_state(update).await?;
            Ok(())
        })
    }
}

async fn move_auth_keys_out_of_sqlite(
    path: &Path,
    secrets: &impl SecretStore,
) -> Result<(), SessionStoreError> {
    if !path.exists() {
        return Ok(());
    }
    let database = libsql::Builder::new_local(path).build().await?.connect()?;
    let statement = match database
        .prepare("SELECT dc_id, auth_key FROM dc_option WHERE auth_key IS NOT NULL")
        .await
    {
        Ok(statement) => statement,
        Err(error) if error.to_string().contains("no such table") => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut rows = statement.query(params![]).await?;
    let mut found = Vec::new();
    while let Some(row) = rows.next().await? {
        let dc_id = row.get::<i32>(0)?;
        let key = row.get::<Vec<u8>>(1)?;
        found.push((dc_id, key));
    }
    drop(rows);
    drop(statement);
    for (dc_id, key) in found {
        let key: [u8; 256] = key.try_into().map_err(|key: Vec<u8>| {
            SessionStoreError::Sqlite(format!(
                "ключ в файле сессии для дата-центра {dc_id} имеет длину {}",
                key.len()
            ))
        })?;
        secrets::put_auth_key(secrets, dc_id, &key)?;
        database
            .execute(
                "UPDATE dc_option SET auth_key = NULL WHERE dc_id = ?",
                params![dc_id],
            )
            .await?;
    }
    drop(database);
    Ok(())
}

pub fn delete_session_files(path: &Path) -> Result<(), SessionStoreError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    if parent.is_dir() {
        for entry in fs::read_dir(parent)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(file_name) {
                fs::remove_file(entry.path())?;
            }
        }
    }
    Ok(())
}

fn restrict_session_files(path: &Path) -> Result<(), SessionStoreError> {
    let Some(parent) = path.parent() else {
        return restrict_owner(path, false);
    };
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return restrict_owner(path, false);
    };
    if parent.is_dir() {
        for entry in fs::read_dir(parent)? {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(file_name) {
                restrict_owner(&entry.path(), false)?;
            }
        }
    }
    Ok(())
}

fn restrict_owner(path: &Path, directory: bool) -> Result<(), SessionStoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if directory { 0o700 } else { 0o600 };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(windows)]
    {
        let _ = directory;
        let output = std::process::Command::new("whoami")
            .output()
            .map_err(|error| SessionStoreError::Io(error.to_string()))?;
        if !output.status.success() {
            return Err(SessionStoreError::Io(
                "не удалось определить текущего пользователя".to_string(),
            ));
        }
        let user = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if user.is_empty() {
            return Err(SessionStoreError::Io(
                "не удалось определить текущего пользователя".to_string(),
            ));
        }
        let status = std::process::Command::new("icacls")
            .arg(path)
            .arg("/inheritance:r")
            .arg("/grant:r")
            .arg(format!("{user}:(F)"))
            .status()
            .map_err(|error| SessionStoreError::Io(error.to_string()))?;
        if !status.success() {
            return Err(SessionStoreError::Io(
                "не удалось ограничить доступ к файлу сессии".to_string(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{store_api_credentials, MemorySecretStore, SecretStore};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "telegram-mcp-session-{}-{nanos}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn assert_bytes_absent(dir: &Path, secret: &[u8]) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if !entry.file_type().unwrap().is_file() {
                continue;
            }
            let bytes = fs::read(entry.path()).unwrap();
            let found = bytes.windows(secret.len()).any(|window| window == secret);
            assert!(
                !found,
                "секрет попал в {}",
                entry.file_name().to_string_lossy()
            );
        }
    }

    #[tokio::test]
    async fn auth_key_is_not_written_to_sqlite() {
        let dir = scratch_dir();
        let path = dir.join("session.sqlite");
        let secrets = MemorySecretStore::default();
        let session = KeychainSession::open(&path, secrets.clone()).await.unwrap();
        let mut option = session.dc_option(2).unwrap().unwrap();
        let key = [0x5A; 256];
        option.auth_key = Some(key);
        session.set_dc_option(&option).await.unwrap();
        let loaded = session.dc_option(2).unwrap().unwrap();
        assert_eq!(loaded.auth_key, Some(key));
        drop(session);

        assert_bytes_absent(&dir, &key);
        assert_bytes_absent(&dir, standard_base64(&key).as_bytes());
        let column = auth_key_column(&path, 2).await;
        assert!(column.is_none());

        let reopened = KeychainSession::open(&path, secrets.clone()).await.unwrap();
        assert_eq!(reopened.dc_option(2).unwrap().unwrap().auth_key, Some(key));
        drop(reopened);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sqlite_auth_key_is_moved_to_the_secret_store() {
        let dir = scratch_dir();
        let path = dir.join("session.sqlite");
        let secrets = MemorySecretStore::default();
        let session = KeychainSession::open(&path, secrets.clone()).await.unwrap();
        let option = session.dc_option(2).unwrap().unwrap();
        session.set_dc_option(&option).await.unwrap();
        drop(session);

        let planted = [0x3C; 256];
        plant_auth_key(&path, 2, &planted).await;
        let moved = KeychainSession::open(&path, secrets.clone()).await.unwrap();
        assert_eq!(moved.dc_option(2).unwrap().unwrap().auth_key, Some(planted));
        drop(moved);
        assert!(auth_key_column(&path, 2).await.is_none());
        assert_eq!(
            secrets.get("auth-key-2").unwrap().unwrap(),
            planted.to_vec()
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn api_hash_stays_out_of_the_session_file() {
        let dir = scratch_dir();
        let path = dir.join("session.sqlite");
        let secrets = MemorySecretStore::default();
        let session = KeychainSession::open(&path, secrets.clone()).await.unwrap();
        drop(session);
        let api_hash = "test-api-hash-9f3c1a";
        store_api_credentials(&secrets, 42, api_hash).unwrap();
        assert_bytes_absent(&dir, api_hash.as_bytes());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn data_dir_uses_the_application_identifier() {
        let path = app_data_dir().unwrap();
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some(APP_DIR_NAME)
        );
    }

    async fn auth_key_column(path: &Path, dc_id: i32) -> Option<Vec<u8>> {
        let database = libsql::Builder::new_local(path)
            .build()
            .await
            .unwrap()
            .connect()
            .unwrap();
        let mut rows = database
            .query(
                "SELECT auth_key FROM dc_option WHERE dc_id = ?",
                params![dc_id],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        row.get::<Option<Vec<u8>>>(0).unwrap()
    }

    async fn plant_auth_key(path: &Path, dc_id: i32, key: &[u8; 256]) {
        let database = libsql::Builder::new_local(path)
            .build()
            .await
            .unwrap()
            .connect()
            .unwrap();
        database
            .execute(
                "UPDATE dc_option SET auth_key = ? WHERE dc_id = ?",
                params![key.to_vec(), dc_id],
            )
            .await
            .unwrap();
    }

    fn standard_base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut output = String::new();
        let mut index = 0;
        while index + 3 <= bytes.len() {
            let chunk = u32::from(bytes[index]) << 16
                | u32::from(bytes[index + 1]) << 8
                | u32::from(bytes[index + 2]);
            output.push(ALPHABET[((chunk >> 18) & 0x3f) as usize] as char);
            output.push(ALPHABET[((chunk >> 12) & 0x3f) as usize] as char);
            output.push(ALPHABET[((chunk >> 6) & 0x3f) as usize] as char);
            output.push(ALPHABET[(chunk & 0x3f) as usize] as char);
            index += 3;
        }
        output
    }
}
