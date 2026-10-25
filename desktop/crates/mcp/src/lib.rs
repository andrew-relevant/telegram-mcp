//! MCP server.
//!
//! `initialize` and `tools/list` are answered from memory. Only a tool call
//! reaches Telegram, and that path is added with the tools themselves.

use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::{serve_server, ServerHandler};
use telegram_mcp_ipc::{default_endpoint, listen, Listener};

/// One server per connection. Nothing here waits on the Telegram client.
pub struct TelegramServer;

impl ServerHandler for TelegramServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("Telegram MCP", env!("CARGO_PKG_VERSION")),
        )
    }
}

pub async fn serve(listener: &mut Listener) -> Result<(), String> {
    loop {
        let stream = listener
            .accept()
            .await
            .map_err(|error| format!("не удалось принять подключение: {error}"))?;
        tokio::spawn(async move {
            match serve_server(TelegramServer, stream).await {
                Ok(running) => {
                    if running.waiting().await.is_err() {
                        eprintln!("сессия MCP завершилась с ошибкой");
                    }
                }
                Err(_error) => eprintln!("сессия MCP не началась"),
            }
        });
    }
}

pub async fn serve_default() -> Result<(), String> {
    let endpoint = default_endpoint().map_err(|error| error.to_string())?;
    let mut listener = listen(&endpoint).await.map_err(|error| error.to_string())?;
    serve(&mut listener).await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rmcp::ServiceExt;
    use telegram_mcp_ipc::{connect, listen, Endpoint};

    use super::*;

    #[tokio::test]
    async fn initialize_and_tools_list_do_not_wait() {
        let scratch = scratch_dir();
        let endpoint = Endpoint::in_dir(&scratch);
        let mut listener = listen(&endpoint).await.unwrap();
        tokio::spawn(async move {
            let _ = serve(&mut listener).await;
        });

        let handshake = async {
            let stream = connect(&endpoint).await.unwrap();
            let client = ().serve(stream).await.unwrap();
            let info = client.peer_info().expect("initialize");
            assert_eq!(
                info.server_info.as_ref().map(|server| server.name.as_str()),
                Some("Telegram MCP")
            );
            assert!(info.capabilities.tools.is_some());
            let tools = client.list_tools(None).await.unwrap();
            assert!(tools.tools.is_empty());

            let second = connect(&endpoint).await.unwrap();
            let second = ().serve(second).await.unwrap();
            assert!(second.list_tools(None).await.unwrap().tools.is_empty());
        };
        tokio::time::timeout(Duration::from_secs(2), handshake)
            .await
            .expect("initialize answered immediately");
        let _ = std::fs::remove_dir_all(&scratch);
    }

    fn scratch_dir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tm{}{id}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
