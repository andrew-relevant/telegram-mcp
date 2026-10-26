//! Whether a tool may send, and the logged-in client the tools call.
//!
//! The switch defaults to off. Tool names stay in `tools/list` either way.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use telegram_mcp_core::chats::MessageRecord;
use telegram_mcp_core::live::Telegram;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

const INCOMING_LIMIT: usize = 50;

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

struct IncomingFeed {
    recent: Mutex<VecDeque<MessageRecord>>,
    ticks: broadcast::Sender<()>,
}

impl IncomingFeed {
    fn new() -> Self {
        let (ticks, _) = broadcast::channel(32);
        Self {
            recent: Mutex::new(VecDeque::new()),
            ticks,
        }
    }

    fn record(&self, message: MessageRecord) {
        let mut recent = self
            .recent
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        recent.push_back(message);
        while recent.len() > INCOMING_LIMIT {
            recent.pop_front();
        }
        let _ = self.ticks.send(());
    }

    fn snapshot(&self) -> Vec<MessageRecord> {
        self.recent
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    fn subscribe(&self) -> broadcast::Receiver<()> {
        self.ticks.subscribe()
    }
}

pub struct AppState {
    pub gate: SendGate,
    telegram: RwLock<Option<Arc<dyn Telegram>>>,
    incoming: IncomingFeed,
    legacy: Mutex<Vec<Arc<AtomicBool>>>,
    update_task: Mutex<Option<JoinHandle<()>>>,
}

impl AppState {
    pub fn new(gate: SendGate) -> Arc<Self> {
        Arc::new(Self {
            gate,
            telegram: RwLock::new(None),
            incoming: IncomingFeed::new(),
            legacy: Mutex::new(Vec::new()),
            update_task: Mutex::new(None),
        })
    }

    pub fn set_telegram(&self, telegram: Option<Arc<dyn Telegram>>) {
        if telegram.is_none() {
            self.stop_updates();
        }
        *self
            .telegram
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = telegram;
    }

    pub fn set_update_task(&self, task: JoinHandle<()>) {
        self.stop_updates();
        *self
            .update_task
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(task);
    }

    pub fn record_incoming(&self, message: MessageRecord) {
        self.incoming.record(message);
    }

    pub(crate) fn incoming_messages(&self) -> Vec<MessageRecord> {
        self.incoming.snapshot()
    }

    pub(crate) fn subscribe_incoming(&self) -> broadcast::Receiver<()> {
        self.incoming.subscribe()
    }

    pub(crate) fn track_legacy(&self, alive: Arc<AtomicBool>) {
        self.legacy
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(alive);
    }

    pub(crate) fn stop_legacy(&self) {
        let flags = std::mem::take(
            &mut *self
                .legacy
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
        );
        for flag in flags {
            flag.store(false, Ordering::Relaxed);
        }
    }

    fn stop_updates(&self) {
        if let Some(task) = self
            .update_task
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
        {
            task.abort();
        }
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
