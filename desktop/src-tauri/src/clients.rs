//! Cursor, Claude, and ChatGPT desktop config edits.
//!
//! Only the `telegram` block is added, replaced, or removed. A file that
//! cannot be parsed is left untouched.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use toml_edit::{Array, DocumentMut, Item, Value as TomlValue};

pub const RESTART_CLIENT: &str = "Полностью перезапустите клиент.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientKind {
    Cursor,
    Claude,
    Chatgpt,
}

impl ClientKind {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "cursor" => Some(Self::Cursor),
            "claude" => Some(Self::Claude),
            "chatgpt" => Some(Self::Chatgpt),
            _ => None,
        }
    }
}

pub struct ClientFiles {
    pub cursor: PathBuf,
    pub claude: PathBuf,
    pub chatgpt: PathBuf,
}

impl ClientFiles {
    pub fn path(&self, kind: ClientKind) -> &Path {
        match kind {
            ClientKind::Cursor => &self.cursor,
            ClientKind::Claude => &self.claude,
            ClientKind::Chatgpt => &self.chatgpt,
        }
    }
}

pub fn client_files(
    home: &Path,
    appdata: &Path,
    local_app_data: &Path,
    codex_home: Option<&Path>,
    windows: bool,
) -> ClientFiles {
    ClientFiles {
        cursor: home.join(".cursor").join("mcp.json"),
        claude: claude_config_file(home, appdata, local_app_data, windows),
        chatgpt: codex_home
            .unwrap_or(&home.join(".codex"))
            .join("config.toml"),
    }
}

pub fn claude_config_file(
    home: &Path,
    appdata: &Path,
    local_app_data: &Path,
    windows: bool,
) -> PathBuf {
    if windows {
        if let Some(dir) = msix_claude_dir(local_app_data) {
            return dir.join("claude_desktop_config.json");
        }
        return appdata.join("Claude").join("claude_desktop_config.json");
    }
    home.join("Library")
        .join("Application Support")
        .join("Claude")
        .join("claude_desktop_config.json")
}

fn msix_claude_dir(local_app_data: &Path) -> Option<PathBuf> {
    let mut found = Vec::new();
    let entries = fs::read_dir(local_app_data.join("Packages")).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("Claude_") && entry.path().is_dir() {
            found.push(entry.path());
        }
    }
    found.sort();
    found
        .into_iter()
        .next()
        .map(|dir| dir.join("LocalCache").join("Roaming").join("Claude"))
}

pub fn shim_beside(app_exe: &Path) -> PathBuf {
    let name = if cfg!(windows) {
        "telegram-mcp.exe"
    } else {
        "telegram-mcp"
    };
    app_exe
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(name)
}

pub fn applications_install(exe: &Path, home: &Path) -> bool {
    exe.starts_with("/Applications") || exe.starts_with(home.join("Applications"))
}

pub fn refuse_uninstalled(macos: bool, exe: &Path, home: &Path) -> Result<(), String> {
    if macos && !applications_install(exe, home) {
        return Err(
            "Откройте Telegram MCP из /Applications или ~/Applications и нажмите «Подключить» снова."
                .to_owned(),
        );
    }
    Ok(())
}

pub fn connect_client(kind: ClientKind, path: &Path, shim: &Path) -> Result<String, String> {
    let shim = path_text(shim);
    match kind {
        ClientKind::Cursor | ClientKind::Claude => edit_json(path, true, &shim)?,
        ClientKind::Chatgpt => edit_toml(path, true, &shim)?,
    }
    Ok(format!("Записано. {RESTART_CLIENT}"))
}

pub fn disconnect_client(kind: ClientKind, path: &Path) -> Result<String, String> {
    if !path.exists() {
        return Ok(format!("Блок telegram не найден. {RESTART_CLIENT}"));
    }
    match kind {
        ClientKind::Cursor | ClientKind::Claude => edit_json(path, false, "")?,
        ClientKind::Chatgpt => edit_toml(path, false, "")?,
    }
    Ok(format!("Блок telegram удалён. {RESTART_CLIENT}"))
}

pub fn telegram_configured(kind: ClientKind, path: &Path) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let text = fs::read_to_string(path).map_err(|_| read_error(path))?;
    match kind {
        ClientKind::Cursor | ClientKind::Claude => {
            let value: Value =
                serde_json::from_str(&text).map_err(|error| parse_error(path, &error))?;
            Ok(value
                .get("mcpServers")
                .and_then(|servers| servers.get("telegram"))
                .is_some())
        }
        ClientKind::Chatgpt => {
            let doc: DocumentMut = text.parse().map_err(|error| parse_error(path, &error))?;
            Ok(doc
                .get("mcp_servers")
                .and_then(|servers| servers.get("telegram"))
                .is_some())
        }
    }
}

fn edit_json(path: &Path, enable: bool, shim: &str) -> Result<(), String> {
    let mut value = if path.exists() {
        let text = fs::read_to_string(path).map_err(|_| read_error(path))?;
        serde_json::from_str(&text).map_err(|error| parse_error(path, &error))?
    } else if enable {
        Value::Object(serde_json::Map::new())
    } else {
        return Ok(());
    };
    let root = value
        .as_object_mut()
        .ok_or_else(|| format!("{}: корень конфигурации не объект", path.display()))?;
    let servers = root.entry("mcpServers").or_insert_with(|| json!({}));
    let servers = servers
        .as_object_mut()
        .ok_or_else(|| format!("{}: mcpServers не объект", path.display()))?;
    if enable {
        servers.insert(
            "telegram".to_owned(),
            json!({
                "command": shim,
                "args": ["--stdio"]
            }),
        );
    } else {
        servers.remove("telegram");
    }
    write_text(path, &serde_json::to_string_pretty(&value).expect("json"))
}

fn edit_toml(path: &Path, enable: bool, shim: &str) -> Result<(), String> {
    let mut doc = if path.exists() {
        let text = fs::read_to_string(path).map_err(|_| read_error(path))?;
        text.parse::<DocumentMut>()
            .map_err(|error| parse_error(path, &error))?
    } else if enable {
        DocumentMut::new()
    } else {
        return Ok(());
    };
    if let Some(table) = doc.get_mut("mcp_servers").and_then(Item::as_table_mut) {
        table.remove("telegram");
    }
    if enable {
        let servers = doc["mcp_servers"].or_insert(Item::Table(toml_edit::Table::new()));
        let Some(servers) = servers.as_table_mut() else {
            return Err(format!("{}: mcp_servers не таблица", path.display()));
        };
        let mut telegram = toml_edit::Table::new();
        telegram.insert("command", Item::Value(TomlValue::from(shim)));
        let mut args = Array::new();
        args.push("--stdio");
        telegram.insert("args", Item::Value(TomlValue::Array(args)));
        servers.insert("telegram", Item::Table(telegram));
    }
    write_text(path, &doc.to_string())
}

fn write_text(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| write_error(path))?;
    }
    if path.exists() {
        fs::copy(path, backup_path(path))
            .map_err(|_| format!("не удалось сохранить копию {}", path.display()))?;
    }
    let temporary = temporary_path(path);
    fs::write(&temporary, contents).map_err(|_| write_error(path))?;
    fs::rename(&temporary, path).map_err(|_| write_error(path))?;
    Ok(())
}

fn backup_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    path.with_file_name(format!("{name}.bak"))
}

fn temporary_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    path.with_file_name(format!("{name}.tmp"))
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn read_error(path: &Path) -> String {
    format!("не удалось прочитать {}", path.display())
}

fn write_error(path: &Path) -> String {
    format!("не удалось записать {}", path.display())
}

fn parse_error(path: &Path, error: &dyn std::fmt::Display) -> String {
    format!("не удалось разобрать {}: {error}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("cfg{}{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn splices_other_servers_and_keeps_toml_comments() {
        let dir = scratch("splice");
        let cursor = dir.join("mcp.json");
        let claude = dir.join("claude.json");
        let chatgpt = dir.join("config.toml");
        fs::write(
            &cursor,
            r#"{
  "mcpServers": {
    "other": { "command": "other", "args": ["keep"] }
  },
  "theme": "dark"
}
"#,
        )
        .unwrap();
        fs::write(&claude, r#"{"mcpServers":{"notes":{"command":"notes"}}}"#).unwrap();
        fs::write(
            &chatgpt,
            "# keep me\n[mcp_servers.other]\ncommand = \"other\"\n\n[settings]\ntheme = \"dark\"\n",
        )
        .unwrap();
        let shim = Path::new("/Applications/Telegram MCP.app/Contents/MacOS/telegram-mcp");
        for (kind, path) in [
            (ClientKind::Cursor, cursor.as_path()),
            (ClientKind::Claude, claude.as_path()),
            (ClientKind::Chatgpt, chatgpt.as_path()),
        ] {
            let message = connect_client(kind, path, shim).unwrap();
            assert!(message.contains(RESTART_CLIENT));
            assert!(telegram_configured(kind, path).unwrap());
        }

        let cursor_value: Value =
            serde_json::from_str(&fs::read_to_string(&cursor).unwrap()).unwrap();
        assert_eq!(
            cursor_value["mcpServers"]["other"]["command"],
            json!("other")
        );
        assert_eq!(cursor_value["theme"], json!("dark"));
        assert_eq!(
            cursor_value["mcpServers"]["telegram"]["args"],
            json!(["--stdio"])
        );
        let keys: Vec<_> = cursor_value["mcpServers"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(keys, ["other", "telegram"]);

        let claude_text = fs::read_to_string(&claude).unwrap();
        assert!(claude_text.contains("notes"));
        assert!(claude_text.contains("telegram-mcp"));

        let toml = fs::read_to_string(&chatgpt).unwrap();
        assert!(toml.contains("# keep me"));
        assert!(toml.contains("command = \"other\""));
        assert!(toml.contains("theme = \"dark\""));
        assert!(toml.contains("telegram-mcp"));

        disconnect_client(ClientKind::Cursor, &cursor).unwrap();
        disconnect_client(ClientKind::Chatgpt, &chatgpt).unwrap();
        let cursor_value: Value =
            serde_json::from_str(&fs::read_to_string(&cursor).unwrap()).unwrap();
        assert!(cursor_value["mcpServers"].get("telegram").is_none());
        assert!(cursor_value["mcpServers"].get("other").is_some());
        let toml = fs::read_to_string(&chatgpt).unwrap();
        assert!(!toml.contains("telegram"));
        assert!(toml.contains("# keep me"));
        assert!(toml.contains("[mcp_servers.other]"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_config_is_not_overwritten() {
        let dir = scratch("broken");
        let cursor = dir.join("mcp.json");
        let chatgpt = dir.join("config.toml");
        fs::write(&cursor, "{ not json").unwrap();
        fs::write(&chatgpt, "mcp_servers = [\n").unwrap();
        let before_json = fs::read(&cursor).unwrap();
        let before_toml = fs::read(&chatgpt).unwrap();
        let json_error =
            connect_client(ClientKind::Cursor, &cursor, Path::new("shim")).unwrap_err();
        let toml_error =
            connect_client(ClientKind::Chatgpt, &chatgpt, Path::new("shim")).unwrap_err();
        assert!(json_error.contains(&cursor.display().to_string()));
        assert!(toml_error.contains(&chatgpt.display().to_string()));
        assert_eq!(fs::read(&cursor).unwrap(), before_json);
        assert_eq!(fs::read(&chatgpt).unwrap(), before_toml);
        assert!(!backup_path(&cursor).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn windows_claude_prefers_the_package_cache() {
        let dir = scratch("claude");
        let appdata = dir.join("Roaming");
        let local = dir.join("Local");
        let package = local
            .join("Packages")
            .join("Claude_example")
            .join("LocalCache")
            .join("Roaming")
            .join("Claude");
        fs::create_dir_all(&package).unwrap();
        let chosen = claude_config_file(dir.as_path(), &appdata, &local, true);
        assert_eq!(chosen, package.join("claude_desktop_config.json"));

        let plain = scratch("plain");
        let chosen = claude_config_file(
            plain.as_path(),
            &plain.join("Roaming"),
            &plain.join("Local"),
            true,
        );
        assert_eq!(
            chosen,
            plain
                .join("Roaming")
                .join("Claude")
                .join("claude_desktop_config.json")
        );
        let mac = claude_config_file(plain.as_path(), &appdata, &local, false);
        assert!(mac.ends_with("Library/Application Support/Claude/claude_desktop_config.json"));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&plain);
    }

    #[test]
    fn macos_connect_requires_applications() {
        let home = Path::new("/Users/example");
        let installed = Path::new("/Applications/Telegram MCP.app/Contents/MacOS/Telegram MCP");
        let home_installed =
            Path::new("/Users/example/Applications/Telegram MCP.app/Contents/MacOS/Telegram MCP");
        let downloads =
            Path::new("/Users/example/Downloads/Telegram MCP.app/Contents/MacOS/Telegram MCP");
        assert!(refuse_uninstalled(true, installed, home).is_ok());
        assert!(refuse_uninstalled(true, home_installed, home).is_ok());
        assert!(refuse_uninstalled(true, downloads, home).is_err());
        assert!(refuse_uninstalled(false, downloads, home).is_ok());
    }
}
