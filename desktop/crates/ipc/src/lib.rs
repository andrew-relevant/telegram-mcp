//! Local socket path, permissions, listen, and connect.
//!
//! This crate stays free of the Telegram client so the stdio shim can start quickly.
//! macOS uses a user-only unix socket in the app data directory. Windows uses a
//! named pipe whose ACL grants the current user and nobody else.

use std::io;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::{connect, listen, Listener, Stream};
#[cfg(windows)]
pub use windows::{connect, listen, Listener, Stream};

pub const APP_DIR_NAME: &str = "app.telegram-mcp.desktop";
pub const SOCKET_NAME: &str = "mcp.sock";

#[derive(Debug)]
pub enum IpcError {
    MissingDataDir,
    AlreadyRunning,
    Io(io::Error),
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingDataDir => formatter.write_str("каталог данных приложения недоступен"),
            Self::AlreadyRunning => formatter.write_str("сокет Telegram MCP уже занят"),
            Self::Io(error) => write!(formatter, "сокет Telegram MCP: {error}"),
        }
    }
}

impl std::error::Error for IpcError {}

impl From<io::Error> for IpcError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Unix socket or named pipe for one local user.
#[derive(Clone, Debug)]
pub struct Endpoint {
    #[cfg(unix)]
    pub(crate) path: PathBuf,
    #[cfg(windows)]
    pub(crate) name: std::ffi::OsString,
}

impl Endpoint {
    pub fn in_dir(data_dir: &Path) -> Self {
        #[cfg(unix)]
        {
            Self {
                path: data_dir.join(SOCKET_NAME),
            }
        }
        #[cfg(windows)]
        {
            Self {
                name: windows::pipe_name_for_test(data_dir),
            }
        }
    }
}

pub fn app_data_dir() -> Result<PathBuf, IpcError> {
    dirs::data_dir()
        .map(|path| path.join(APP_DIR_NAME))
        .ok_or(IpcError::MissingDataDir)
}

pub fn default_endpoint() -> Result<Endpoint, IpcError> {
    #[cfg(unix)]
    {
        Ok(Endpoint::in_dir(&app_data_dir()?))
    }
    #[cfg(windows)]
    {
        windows::user_endpoint()
    }
}

/// Copy bytes both ways until each reader reaches EOF.
///
/// The shim does not parse MCP. Shutting a write half down when its reader
/// ends avoids a stall where each side waits for the other to close.
pub async fn bridge<R, W, S>(mut reader: R, mut writer: W, socket: S) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (mut socket_read, mut socket_write) = tokio::io::split(socket);
    let to_socket = async {
        let copied = tokio::io::copy(&mut reader, &mut socket_write).await;
        let shutdown = socket_write.shutdown().await;
        copied?;
        shutdown?;
        Ok::<(), io::Error>(())
    };
    let to_client = async {
        let copied = tokio::io::copy(&mut socket_read, &mut writer).await;
        let shutdown = writer.shutdown().await;
        copied?;
        shutdown?;
        Ok::<(), io::Error>(())
    };
    tokio::try_join!(to_socket, to_client)?;
    Ok(())
}

/// DACL that allows the given user SID and no one else.
///
/// The SID must already be a Windows SID (`S-1-...`). Anything else is refused
/// so a crafted value cannot append another ACE.
#[cfg(any(windows, test))]
pub(crate) fn owner_only_sddl(sid: &str) -> Option<String> {
    let mut chars = sid.chars();
    if chars.next() != Some('S') || chars.next() != Some('-') {
        return None;
    }
    let mut saw_digit = false;
    for character in chars {
        if character.is_ascii_digit() {
            saw_digit = true;
        } else if character != '-' {
            return None;
        }
    }
    if !saw_digit {
        return None;
    }
    Some(format!("D:P(A;;GA;;;{sid})"))
}

#[cfg(unix)]
pub(crate) fn set_owner_only(path: &Path, directory: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if directory { 0o700 } else { 0o600 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_only_sddl_names_the_user_sid() {
        assert_eq!(
            owner_only_sddl("S-1-5-21-9").as_deref(),
            Some("D:P(A;;GA;;;S-1-5-21-9)")
        );
        assert!(owner_only_sddl("S-1-5);WD").is_none());
        assert!(owner_only_sddl("everyone").is_none());
    }

    #[test]
    fn default_endpoint_stays_inside_the_app_data_dir() {
        let endpoint = default_endpoint().unwrap();
        #[cfg(unix)]
        {
            assert_eq!(
                endpoint.path.file_name().and_then(|name| name.to_str()),
                Some(SOCKET_NAME)
            );
            assert_eq!(
                endpoint
                    .path
                    .parent()
                    .and_then(|path| path.file_name())
                    .and_then(|name| name.to_str()),
                Some(APP_DIR_NAME)
            );
        }
        #[cfg(windows)]
        {
            let name = endpoint.name.to_string_lossy();
            assert!(name.starts_with(&format!(r"\\.\pipe\{APP_DIR_NAME}-S-")));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_is_owner_only_and_replaces_a_stale_file() {
        let scratch = scratch_dir();
        let endpoint = Endpoint::in_dir(&scratch.path);
        std::fs::write(&endpoint.path, b"stale").unwrap();

        let listener = listen(&endpoint).await.unwrap();
        let mode = file_mode(&endpoint.path);
        assert_eq!(mode, 0o600, "socket mode is {mode:o}");
        assert_eq!(file_mode(&scratch.path), 0o700);

        match listen(&endpoint).await {
            Err(IpcError::AlreadyRunning) => {}
            Err(error) => panic!("expected the socket to be busy, got {error}"),
            Ok(_) => panic!("expected the socket to be busy"),
        }
        drop(listener);
    }

    #[cfg(unix)]
    fn file_mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    struct ScratchDir {
        path: PathBuf,
    }

    #[cfg(unix)]
    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[cfg(unix)]
    fn scratch_dir() -> ScratchDir {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        // macOS unix sockets reject paths longer than 104 bytes.
        let path = std::env::temp_dir().join(format!("tg{}{id}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        ScratchDir { path }
    }
}
