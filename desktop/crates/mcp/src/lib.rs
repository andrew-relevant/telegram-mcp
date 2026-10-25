//! MCP server.
//!
//! `initialize` and `tools/list` are answered from memory. Only a tool call
//! reaches Telegram.

mod state;
mod tools;

use std::future::Future;
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{serve_server, ErrorData, RoleServer, ServerHandler};
use telegram_mcp_ipc::{app_data_dir, default_endpoint, listen, Listener};

pub use state::{AppState, SendGate};

/// One server per connection. Listing tools does not wait on Telegram.
pub struct TelegramServer {
    state: Arc<AppState>,
}

impl ServerHandler for TelegramServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("Telegram MCP", env!("CARGO_PKG_VERSION")),
        )
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(tools::definitions())))
    }

    fn get_tool(&self, name: &str) -> Option<rmcp::model::Tool> {
        tools::find(name)
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + Send + '_ {
        let state = Arc::clone(&self.state);
        async move {
            let result =
                tools::invoke(&state, request.name.as_ref(), request.arguments.as_ref()).await;
            Ok(CallToolResponse::Complete(result))
        }
    }
}

pub async fn serve(listener: &mut Listener, state: Arc<AppState>) -> Result<(), String> {
    loop {
        let stream = listener
            .accept()
            .await
            .map_err(|error| format!("не удалось принять подключение: {error}"))?;
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            match serve_server(TelegramServer { state }, stream).await {
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

pub async fn serve_with(state: Arc<AppState>) -> Result<(), String> {
    let endpoint = default_endpoint().map_err(|error| error.to_string())?;
    let mut listener = listen(&endpoint).await.map_err(|error| error.to_string())?;
    serve(&mut listener, state).await
}

pub async fn serve_default() -> Result<(), String> {
    let state = AppState::new(SendGate::load(
        &app_data_dir().map_err(|error| error.to_string())?,
    ));
    serve_with(state).await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rmcp::model::{CallToolRequestParams, JsonObject};
    use rmcp::ServiceExt;
    use serde_json::Value;
    use telegram_mcp_ipc::{connect, listen, Endpoint};

    use super::*;

    #[tokio::test]
    async fn initialize_and_tools_list_do_not_wait() {
        let scratch = scratch_dir();
        let endpoint = Endpoint::in_dir(&scratch);
        let mut listener = listen(&endpoint).await.unwrap();
        let state = AppState::new(SendGate::off());
        tokio::spawn(async move {
            let _ = serve(&mut listener, state).await;
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
            let names: Vec<_> = tools.tools.iter().map(|tool| tool.name.as_ref()).collect();
            assert_eq!(
                names,
                [
                    "list_chats",
                    "get_chat",
                    "get_history",
                    "search_messages",
                    "search_global",
                    "send_message",
                    "reply_to_message",
                ]
            );
            for tool in &tools.tools {
                let hints = tool.annotations.as_ref().expect("annotations");
                let sending = tool.name == "send_message" || tool.name == "reply_to_message";
                assert_eq!(hints.read_only_hint, Some(!sending));
                assert_eq!(hints.destructive_hint, Some(sending));
            }

            let mut arguments = JsonObject::new();
            arguments.insert("chat_id".into(), Value::from(1));
            arguments.insert("message".into(), Value::from("x"));
            let refused = client
                .call_tool(CallToolRequestParams::new("send_message").with_arguments(arguments))
                .await
                .unwrap();
            let text = refused
                .content
                .first()
                .and_then(|block| block.as_text())
                .map(|block| block.text.as_str())
                .unwrap_or("");
            assert_eq!(text, tools::NOT_LOGGED_IN);

            let second = connect(&endpoint).await.unwrap();
            let second = ().serve(second).await.unwrap();
            assert_eq!(second.list_tools(None).await.unwrap().tools.len(), 7);
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
