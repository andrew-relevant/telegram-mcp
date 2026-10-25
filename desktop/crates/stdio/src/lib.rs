//! Byte bridge from an MCP client's stdin/stdout to the tray socket.
//!
//! MCP is not parsed here. Logs go to stderr. Message text is never written there.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use telegram_mcp_ipc::{bridge, connect, default_endpoint, Endpoint, Stream};

pub const START_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub enum ShimError {
    Usage,
    Timeout(Duration),
    TrayMissing { directory: PathBuf },
    Start(String),
    Bridge(String),
}

impl std::fmt::Display for ShimError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage => formatter.write_str("Использование: telegram-mcp --stdio"),
            Self::Timeout(waited) => {
                if waited.as_secs() == 0 {
                    write!(
                        formatter,
                        "не удалось дождаться Telegram MCP за {} мс",
                        waited.as_millis()
                    )
                } else {
                    write!(
                        formatter,
                        "не удалось дождаться Telegram MCP за {} с",
                        waited.as_secs()
                    )
                }
            }
            Self::TrayMissing { directory } => write!(
                formatter,
                "не найден Telegram MCP рядом с {}",
                directory.display()
            ),
            Self::Start(message) | Self::Bridge(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ShimError {}

impl ShimError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Usage => 2,
            _ => 1,
        }
    }
}

pub fn entry() -> i32 {
    if let Err(error) = parse_args(std::env::args().skip(1)) {
        eprintln!("{error}");
        return error.exit_code();
    }
    let endpoint = match default_endpoint() {
        Ok(endpoint) => endpoint,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("не удалось запустить прокладку: {error}");
            return 1;
        }
    };
    if let Err(error) = runtime.block_on(run(endpoint, START_TIMEOUT)) {
        eprintln!("{error}");
        return error.exit_code();
    }
    0
}

pub async fn run(endpoint: Endpoint, timeout: Duration) -> Result<(), ShimError> {
    let stream = attach(&endpoint, timeout, spawn_current_tray).await?;
    bridge(tokio::io::stdin(), tokio::io::stdout(), stream)
        .await
        .map_err(|error| ShimError::Bridge(error.to_string()))?;
    Ok(())
}

async fn attach<F>(endpoint: &Endpoint, timeout: Duration, start: F) -> Result<Stream, ShimError>
where
    F: FnOnce() -> Result<(), ShimError>,
{
    if let Ok(stream) = connect(endpoint).await {
        return Ok(stream);
    }
    start()?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(stream) = connect(endpoint).await {
            return Ok(stream);
        }
        if Instant::now() >= deadline {
            return Err(ShimError::Timeout(timeout));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn parse_args<I>(args: I) -> Result<(), ShimError>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    match (args.next().as_deref(), args.next()) {
        (Some("--stdio"), None) => Ok(()),
        _ => Err(ShimError::Usage),
    }
}

fn spawn_current_tray() -> Result<(), ShimError> {
    let executable =
        std::env::current_exe().map_err(|error| ShimError::Start(error.to_string()))?;
    spawn_tray(&executable)
}

fn spawn_tray(shim_executable: &Path) -> Result<(), ShimError> {
    let Some(directory) = shim_executable.parent() else {
        return Err(ShimError::TrayMissing {
            directory: PathBuf::new(),
        });
    };
    let Some(tray) = tray_candidates(directory)
        .into_iter()
        .find(|path| path.is_file())
    else {
        return Err(ShimError::TrayMissing {
            directory: directory.to_path_buf(),
        });
    };
    let mut command = Command::new(&tray);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP. The tray must outlive the shim.
        command.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    let mut child = command
        .spawn()
        .map_err(|error| ShimError::Start(error.to_string()))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn tray_candidates(directory: &Path) -> [PathBuf; 2] {
    let names: [&str; 2] = if cfg!(windows) {
        ["Telegram MCP.exe", "telegram-mcp-desktop.exe"]
    } else {
        ["Telegram MCP", "telegram-mcp-desktop"]
    };
    [directory.join(names[0]), directory.join(names[1])]
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use telegram_mcp_ipc::{bridge, listen};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[test]
    fn only_stdio_is_accepted() {
        assert!(parse_args(["--stdio".to_owned()]).is_ok());
        let error = parse_args(std::iter::empty()).unwrap_err();
        assert!(matches!(error, ShimError::Usage));
        assert_eq!(error.exit_code(), 2);
        assert!(parse_args(["--stdio".to_owned(), "--extra".to_owned()]).is_err());
    }

    #[test]
    fn missing_tray_is_reported_without_starting_a_process() {
        let scratch = scratch_dir();
        let error = spawn_tray(&scratch.path.join("telegram-mcp")).unwrap_err();
        match error {
            ShimError::TrayMissing { directory } => assert_eq!(directory, scratch.path),
            other => panic!("expected a missing tray, got {other}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn tray_next_to_the_shim_can_be_started() {
        let scratch = scratch_dir();
        let tray = scratch.path.join("telegram-mcp-desktop");
        std::fs::copy("/usr/bin/true", &tray).unwrap();
        spawn_tray(&scratch.path.join("telegram-mcp")).unwrap();
    }

    #[tokio::test]
    async fn shim_forwards_bytes_to_a_test_socket() {
        let scratch = scratch_dir();
        let endpoint = Endpoint::in_dir(&scratch.path);
        let mut listener = listen(&endpoint).await.unwrap();
        let server = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            let mut received = Vec::new();
            stream.read_to_end(&mut received).await.unwrap();
            stream.write_all(b"from-tray").await.unwrap();
            drop(stream);
            received
        });

        let started = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&started);
        let stream = attach(&endpoint, Duration::from_secs(2), || {
            flag.store(true, Ordering::SeqCst);
            Ok(())
        })
        .await
        .unwrap();
        assert!(!started.load(Ordering::SeqCst));

        let (mut stdin_write, stdin_read) = tokio::io::duplex(64);
        let (mut stdout_read, stdout_write) = tokio::io::duplex(64);
        let bridge_task = tokio::spawn(async move {
            bridge(stdin_read, stdout_write, stream).await.unwrap();
        });
        stdin_write.write_all(b"from-client").await.unwrap();
        drop(stdin_write);

        let received = server.await.unwrap();
        assert_eq!(received, b"from-client");
        let mut output = Vec::new();
        stdout_read.read_to_end(&mut output).await.unwrap();
        assert_eq!(output, b"from-tray");
        bridge_task.await.unwrap();
    }

    #[tokio::test]
    async fn shim_starts_the_tray_when_the_socket_is_down() {
        let scratch = scratch_dir();
        let endpoint = Endpoint::in_dir(&scratch.path);
        let endpoint_for_start = endpoint.clone();
        let started = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&started);
        let stream = attach(&endpoint, Duration::from_secs(2), move || {
            flag.store(true, Ordering::SeqCst);
            let endpoint = endpoint_for_start.clone();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(async move {
                    let listener = listen(&endpoint).await.unwrap();
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    drop(listener);
                });
            });
            Ok(())
        })
        .await
        .unwrap();
        assert!(started.load(Ordering::SeqCst));
        drop(stream);
    }

    #[tokio::test]
    async fn missing_socket_reports_why_it_stopped() {
        let scratch = scratch_dir();
        let endpoint = Endpoint::in_dir(&scratch.path);
        let Err(error) = attach(&endpoint, Duration::from_millis(200), || Ok(())).await else {
            panic!("expected the wait to fail");
        };
        let message = error.to_string();
        assert!(message.contains("не удалось дождаться Telegram MCP"));
        assert!(!message.contains("from-client"));
    }

    struct ScratchDir {
        path: PathBuf,
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn scratch_dir() -> ScratchDir {
        use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, AtomicOrdering::Relaxed);
        let path = std::env::temp_dir().join(format!("ts{}{id}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        ScratchDir { path }
    }
}
