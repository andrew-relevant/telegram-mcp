//! Whether a tool may send, and the logged-in client the tools call.
//!
//! The switch defaults to off. Tool names stay in `tools/list` either way.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use telegram_mcp_core::live::Telegram;

const FLAG_NAME: &str = "send-enabled";

pub struct SendGate {
    enabled: AtomicBool,
    path: Option<PathBuf>,
}

impl SendGate {
    pub fn off() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            path: None,
        }
    }

    pub fn load(dir: &Path) -> Self {
        let path = dir.join(FLAG_NAME);
        let enabled = std::fs::read_to_string(&path)
            .ok()
            .is_some_and(|text| text.trim() == "1");
        Self {
            enabled: AtomicBool::new(enabled),
            path: Some(path),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set(&self, enabled: bool) -> Result<(), String> {
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|_| "не удалось сохранить переключатель отправки".to_owned())?;
            }
            let bytes = if enabled { "1" } else { "0" };
            std::fs::write(path, bytes)
                .map_err(|_| "не удалось сохранить переключатель отправки".to_owned())?;
            restrict(path)?;
        }
        self.enabled.store(enabled, Ordering::Relaxed);
        Ok(())
    }
}

pub struct AppState {
    pub gate: SendGate,
    telegram: RwLock<Option<Arc<dyn Telegram>>>,
}

impl AppState {
    pub fn new(gate: SendGate) -> Arc<Self> {
        Arc::new(Self {
            gate,
            telegram: RwLock::new(None),
        })
    }

    pub fn set_telegram(&self, telegram: Option<Arc<dyn Telegram>>) {
        *self
            .telegram
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = telegram;
    }

    pub(crate) fn telegram(&self) -> Option<Arc<dyn Telegram>> {
        self.telegram
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

fn restrict(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| "не удалось сохранить переключатель отправки".to_owned())?;
    }
    #[cfg(windows)]
    {
        let output = std::process::Command::new("whoami")
            .output()
            .map_err(|_| "не удалось сохранить переключатель отправки".to_owned())?;
        if !output.status.success() {
            return Err("не удалось сохранить переключатель отправки".to_owned());
        }
        let user = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if user.is_empty() {
            return Err("не удалось сохранить переключатель отправки".to_owned());
        }
        let status = std::process::Command::new("icacls")
            .arg(path)
            .arg("/inheritance:r")
            .arg("/grant:r")
            .arg(format!("{user}:(F)"))
            .status()
            .map_err(|_| "не удалось сохранить переключатель отправки".to_owned())?;
        if !status.success() {
            return Err("не удалось сохранить переключатель отправки".to_owned());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_file_is_owner_only() {
        let dir = std::env::temp_dir().join(format!("se{}0", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let gate = SendGate::load(&dir);
        assert!(!gate.is_enabled());
        gate.set(true).unwrap();
        assert!(gate.is_enabled());
        assert_eq!(
            std::fs::read_to_string(dir.join(FLAG_NAME)).unwrap().trim(),
            "1"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(FLAG_NAME))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        let again = SendGate::load(&dir);
        assert!(again.is_enabled());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
