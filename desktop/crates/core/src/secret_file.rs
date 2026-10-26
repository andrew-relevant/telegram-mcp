//! Owner-only secret files.
//!
//! An ad-hoc signed Mac app cannot store a Keychain permission: the login
//! password dialog returns on every read. These files stay out of the SQLite
//! session and are readable only by the current user.

use std::fs;
use std::path::{Path, PathBuf};

use crate::secrets::SecretStoreError;

const DIRECTORY: &str = "secrets";

pub fn get(root: &Path, account: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
    let path = account_path(root, account)?;
    match fs::read(&path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(error)),
    }
}

pub fn set(root: &Path, account: &str, secret: &[u8]) -> Result<(), SecretStoreError> {
    let path = account_path(root, account)?;
    let directory = path
        .parent()
        .ok_or_else(|| SecretStoreError::new("не найден каталог секретов"))?;
    fs::create_dir_all(directory).map_err(io_error)?;
    restrict(directory, true)?;
    let temporary = directory.join(format!(".{account}.tmp"));
    fs::write(&temporary, secret).map_err(io_error)?;
    restrict(&temporary, false)?;
    fs::rename(&temporary, &path).map_err(io_error)?;
    restrict(&path, false)?;
    Ok(())
}

pub fn delete(root: &Path, account: &str) -> Result<(), SecretStoreError> {
    let path = account_path(root, account)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

fn account_path(root: &Path, account: &str) -> Result<PathBuf, SecretStoreError> {
    if account.is_empty()
        || !account
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(SecretStoreError::new("недопустимое имя записи"));
    }
    Ok(root.join(DIRECTORY).join(account))
}

fn restrict(path: &Path, directory: bool) -> Result<(), SecretStoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if directory { 0o700 } else { 0o600 };
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(io_error)?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, directory);
    }
    Ok(())
}

fn io_error(error: std::io::Error) -> SecretStoreError {
    SecretStoreError::new(format!("не удалось записать секрет: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_stays_owner_readable() {
        let root = std::env::temp_dir().join(format!(
            "telegram-mcp-secrets-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        set(&root, "api-hash", b"secret-value").unwrap();
        assert_eq!(get(&root, "api-hash").unwrap().unwrap(), b"secret-value");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(root.join("secrets").join("api-hash"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        delete(&root, "api-hash").unwrap();
        assert!(get(&root, "api-hash").unwrap().is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn account_name_cannot_escape_the_directory() {
        let root = std::env::temp_dir();
        assert!(set(&root, "../api-hash", b"no").is_err());
        assert!(get(&root, "a/b").is_err());
    }
}
