//! Window state: credentials, QR, cloud password, and logout.
//!
//! Saved login data is read only from the background task, never from a test.

use std::sync::{Arc, Mutex};

use serde::Serialize;
use telegram_mcp_core::live::LiveTelegram;
use telegram_mcp_core::qr_login::{
    advance_login, begin_password, connect, login_qr_svg, refresh_wait, try_password,
    wait_for_login_update, Connected, LoginStep, PasswordAttempt,
};
use telegram_mcp_core::secrets::{
    load_api_credentials, store_api_credentials, wipe_secrets, ApiCredentialChange,
    SystemSecretStore,
};
use telegram_mcp_core::secured_session::{session_file_path, KeychainSession};
use telegram_mcp_mcp::AppState;
use tokio::sync::{mpsc, oneshot};

use crate::clients::{
    client_files, connect_client, disconnect_client, refuse_uninstalled, shim_beside,
    telegram_configured, ClientFiles, ClientKind,
};

#[derive(Clone)]
struct Phase {
    screen: &'static str,
    qr_svg: Option<String>,
    password_hint: Option<String>,
    account_name: Option<String>,
    message: Option<String>,
}

impl Phase {
    fn screen(screen: &'static str) -> Self {
        Self {
            screen,
            qr_svg: None,
            password_hint: None,
            account_name: None,
            message: None,
        }
    }
}

enum UiCommand {
    Save {
        api_id: i32,
        api_hash: String,
        replace: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Password {
        password: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Logout {
        reply: oneshot::Sender<Result<(), String>>,
    },
}

enum LoopControl {
    Continue,
    Restart,
}

pub struct Desk {
    state: Arc<AppState>,
    phase: Mutex<Phase>,
    commands: mpsc::UnboundedSender<UiCommand>,
    files: ClientFiles,
    macos: bool,
}

#[derive(Serialize)]
pub struct Status {
    pub screen: String,
    pub qr_svg: Option<String>,
    pub password_hint: Option<String>,
    pub account_name: Option<String>,
    pub message: Option<String>,
    pub sending: bool,
    pub cursor: bool,
    pub claude: bool,
    pub chatgpt: bool,
    pub client_error: Option<String>,
}

impl Desk {
    pub fn start(state: Arc<AppState>, files: ClientFiles, macos: bool) -> Arc<Self> {
        let (commands, inbox) = mpsc::unbounded_channel();
        let desk = Arc::new(Self {
            state: Arc::clone(&state),
            phase: Mutex::new(Phase::screen("credentials")),
            commands,
            files,
            macos,
        });
        tauri::async_runtime::spawn(session_loop(state, Arc::clone(&desk), inbox));
        desk
    }

    pub fn status(&self) -> Status {
        let phase = self
            .phase
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let (cursor, claude, chatgpt, client_error) = client_flags(&self.files);
        Status {
            screen: phase.screen.to_owned(),
            qr_svg: phase.qr_svg.clone(),
            password_hint: phase.password_hint.clone(),
            account_name: phase.account_name.clone(),
            message: phase.message.clone(),
            sending: self.state.gate.is_enabled(),
            cursor,
            claude,
            chatgpt,
            client_error,
        }
    }

    pub async fn save_credentials(
        &self,
        api_id: i32,
        api_hash: String,
        replace: bool,
    ) -> Result<(), String> {
        self.request(|reply| UiCommand::Save {
            api_id,
            api_hash,
            replace,
            reply,
        })
        .await
    }

    pub async fn submit_password(&self, password: String) -> Result<(), String> {
        self.request(|reply| UiCommand::Password { password, reply })
            .await
    }

    pub async fn logout(&self) -> Result<(), String> {
        self.request(|reply| UiCommand::Logout { reply }).await
    }

    pub fn set_sending(&self, enabled: bool) -> Result<(), String> {
        self.state.gate.set(enabled)
    }

    pub fn set_client(&self, kind: ClientKind, enabled: bool) -> Result<String, String> {
        let exe = std::env::current_exe().map_err(|_| "не удалось определить путь приложения")?;
        let home = home_dir()?;
        refuse_uninstalled(self.macos, &exe, &home)?;
        let path = self.files.path(kind);
        if enabled {
            connect_client(kind, path, &shim_beside(&exe))
        } else {
            disconnect_client(kind, path)
        }
    }

    async fn request(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<(), String>>) -> UiCommand,
    ) -> Result<(), String> {
        let (reply, result) = oneshot::channel();
        self.commands
            .send(command(reply))
            .map_err(|_| "приложение не готово".to_owned())?;
        result
            .await
            .map_err(|_| "приложение не готово".to_owned())?
    }

    fn set_phase(&self, phase: Phase) {
        *self
            .phase
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = phase;
    }
}

fn client_flags(files: &ClientFiles) -> (bool, bool, bool, Option<String>) {
    let mut error = None;
    let mut read = |kind, path: &std::path::Path| match telegram_configured(kind, path) {
        Ok(value) => value,
        Err(message) => {
            error.get_or_insert(message);
            false
        }
    };
    let cursor = read(ClientKind::Cursor, &files.cursor);
    let claude = read(ClientKind::Claude, &files.claude);
    let chatgpt = read(ClientKind::Chatgpt, &files.chatgpt);
    (cursor, claude, chatgpt, error)
}

pub fn home_dir() -> Result<std::path::PathBuf, String> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(key)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| "не найден домашний каталог".to_owned())
}

pub fn current_client_files() -> ClientFiles {
    let home = home_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let appdata = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join("Library/Application Support"));
    let local = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join("AppData/Local"));
    let codex = std::env::var_os("CODEX_HOME").map(std::path::PathBuf::from);
    client_files(&home, &appdata, &local, codex.as_deref(), cfg!(windows))
}

struct SavedLogin {
    api_id: i32,
    api_hash: String,
}

async fn session_loop(
    state: Arc<AppState>,
    desk: Arc<Desk>,
    mut inbox: mpsc::UnboundedReceiver<UiCommand>,
) {
    let mut wait_for_click = false;
    loop {
        let Some(saved) = wait_for_credentials(&desk, &mut inbox, wait_for_click).await else {
            return;
        };
        wait_for_click = false;
        match run_session(Arc::clone(&state), &desk, &mut inbox, saved).await {
            Ok(LoopControl::Restart) => desk.set_phase(Phase::screen("credentials")),
            Ok(LoopControl::Continue) => {}
            Err(message) => {
                let mut phase = Phase::screen("credentials");
                phase.message = Some(message);
                desk.set_phase(phase);
                wait_for_click = true;
            }
        }
    }
}

async fn wait_for_credentials(
    desk: &Desk,
    inbox: &mut mpsc::UnboundedReceiver<UiCommand>,
    wait_for_click: bool,
) -> Option<SavedLogin> {
    if !wait_for_click {
        if let Some(saved) = load_saved() {
            return Some(saved);
        }
    }
    desk.set_phase(Phase::screen("credentials"));
    while let Some(command) = inbox.recv().await {
        if let Some(saved) = accept_logged_out(desk, command).await {
            return Some(saved);
        }
    }
    None
}

fn load_saved() -> Option<SavedLogin> {
    let saved = load_api_credentials(&SystemSecretStore).ok()??;
    Some(SavedLogin {
        api_id: saved.api_id,
        api_hash: saved.api_hash,
    })
}

async fn accept_logged_out(desk: &Desk, command: UiCommand) -> Option<SavedLogin> {
    match command {
        UiCommand::Save {
            api_id,
            api_hash,
            replace,
            reply,
        } => {
            if replace {
                let _ = wipe_closed_session().await;
            }
            match store_login(api_id, api_hash) {
                Ok(saved) => {
                    let _ = reply.send(Ok(()));
                    Some(saved)
                }
                Err(message) => {
                    let _ = reply.send(Err(message));
                    None
                }
            }
        }
        UiCommand::Password { reply, .. } => {
            let _ = reply.send(Err("Сначала сохраните api_id и api_hash.".to_owned()));
            None
        }
        UiCommand::Logout { reply } => {
            let _ = wipe_closed_session().await;
            let _ = reply.send(Ok(()));
            desk.set_phase(Phase::screen("credentials"));
            None
        }
    }
}

fn store_login(api_id: i32, api_hash: String) -> Result<SavedLogin, String> {
    let api_hash = api_hash.trim().to_owned();
    if api_id <= 0 || api_hash.is_empty() {
        return Err("Укажите api_id и api_hash.".to_owned());
    }
    match store_api_credentials(&SystemSecretStore, api_id, &api_hash) {
        Ok(ApiCredentialChange::ApiIdChanged { .. }) => {
            Err("Этот api_id от другого приложения. Нажмите «Выйти и записать».".to_owned())
        }
        Ok(_) => Ok(SavedLogin { api_id, api_hash }),
        Err(_) => Err("не удалось сохранить api_id и api_hash".to_owned()),
    }
}

async fn run_session(
    state: Arc<AppState>,
    desk: &Desk,
    inbox: &mut mpsc::UnboundedReceiver<UiCommand>,
    saved: SavedLogin,
) -> Result<LoopControl, String> {
    let path = session_file_path().map_err(|_| "не найден каталог приложения".to_owned())?;
    let session = Arc::new(
        KeychainSession::open(&path, SystemSecretStore)
            .await
            .map_err(|error| error.to_string())?,
    );
    let link = connect(Arc::clone(&session), saved.api_id)
        .await
        .map_err(|_| "не удалось подключиться к Telegram".to_owned())?;
    if link.client.is_authorized().await.unwrap_or(false) {
        return hold_ready(state, desk, inbox, link, session).await;
    }
    login_until_ready(state, desk, inbox, link, session, saved).await
}

async fn login_until_ready(
    state: Arc<AppState>,
    desk: &Desk,
    inbox: &mut mpsc::UnboundedReceiver<UiCommand>,
    mut link: Connected,
    session: Arc<KeychainSession<SystemSecretStore>>,
    mut saved: SavedLogin,
) -> Result<LoopControl, String> {
    loop {
        let step = advance_login(
            &link.client,
            session.as_ref(),
            &link.handle,
            saved.api_id,
            &saved.api_hash,
        )
        .await
        .map_err(|_| "не удалось получить QR".to_owned())?;
        match step {
            LoginStep::Authorized => {
                return hold_ready(Arc::clone(&state), desk, inbox, link, session).await;
            }
            LoginStep::PasswordNeeded => match ask_password(desk, inbox, &link).await? {
                LoopControl::Restart => {
                    let _ = link.client.sign_out().await;
                    end_session(&state, link, session).await;
                    return Ok(LoopControl::Restart);
                }
                LoopControl::Continue => {}
            },
            LoginStep::Qr { token, expires } => {
                show_qr(desk, &token);
                let wait = refresh_wait(expires);
                tokio::select! {
                    command = inbox.recv() => {
                        let Some(command) = command else {
                            return Err("канал окна закрыт".to_owned());
                        };
                        match command_restarts(desk, command, &mut saved)? {
                            QrAction::Stay => {}
                            QrAction::Leave => {
                                let _ = link.client.sign_out().await;
                                end_session(&state, link, session).await;
                                return Ok(LoopControl::Restart);
                            }
                            QrAction::Replace {
                                api_id,
                                api_hash,
                                reply,
                            } => {
                                let _ = link.client.sign_out().await;
                                end_session(&state, link, session).await;
                                let _ = reply.send(store_login(api_id, api_hash).map(|_| ()));
                                return Ok(LoopControl::Restart);
                            }
                        }
                    }
                    _ = wait_for_login_update(link.updates_mut(), wait) => {}
                }
            }
        }
    }
}

fn show_qr(desk: &Desk, token: &[u8]) {
    let mut phase = Phase::screen("qr");
    phase.qr_svg = login_qr_svg(token).ok();
    phase.message = Some(
        "Отсканируйте QR в Telegram: Настройки → Устройства → Подключить устройство.".to_owned(),
    );
    desk.set_phase(phase);
}

async fn ask_password(
    desk: &Desk,
    inbox: &mut mpsc::UnboundedReceiver<UiCommand>,
    link: &Connected,
) -> Result<LoopControl, String> {
    let mut token = begin_password(&link.client)
        .await
        .map_err(|_| "не удалось запросить облачный пароль".to_owned())?;
    let hint = token.hint().map(str::to_owned);
    loop {
        let mut phase = Phase::screen("password");
        phase.password_hint = hint.clone();
        phase.message = Some("Включен облачный пароль.".to_owned());
        desk.set_phase(phase);
        let Some(command) = inbox.recv().await else {
            return Err("канал окна закрыт".to_owned());
        };
        match command {
            UiCommand::Password { password, reply } => {
                if password.is_empty() {
                    let _ = reply.send(Err("Введите облачный пароль.".to_owned()));
                    continue;
                }
                match try_password(&link.client, token, &password).await {
                    Ok(PasswordAttempt::Accepted) => {
                        let _ = reply.send(Ok(()));
                        return Ok(LoopControl::Continue);
                    }
                    Ok(PasswordAttempt::Rejected(again)) => {
                        token = again;
                        let _ = reply.send(Err("Неверный пароль.".to_owned()));
                    }
                    Err(_) => {
                        let _ = reply.send(Err("не удалось проверить пароль".to_owned()));
                        return Err("не удалось проверить пароль".to_owned());
                    }
                }
            }
            UiCommand::Logout { reply } => {
                let _ = reply.send(Ok(()));
                return Ok(LoopControl::Restart);
            }
            UiCommand::Save { reply, .. } => {
                let _ = reply.send(Err(
                    "Сначала выйдите, затем запишите другой api_id.".to_owned()
                ));
            }
        }
    }
}

enum QrAction {
    Stay,
    Leave,
    Replace {
        api_id: i32,
        api_hash: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

fn command_restarts(
    desk: &Desk,
    command: UiCommand,
    saved: &mut SavedLogin,
) -> Result<QrAction, String> {
    match command {
        UiCommand::Logout { reply } => {
            let _ = reply.send(Ok(()));
            desk.set_phase(Phase::screen("credentials"));
            Ok(QrAction::Leave)
        }
        UiCommand::Save {
            api_id,
            api_hash,
            replace,
            reply,
        } => {
            if replace {
                return Ok(QrAction::Replace {
                    api_id,
                    api_hash,
                    reply,
                });
            }
            match store_login(api_id, api_hash) {
                Ok(next) => {
                    *saved = next;
                    let _ = reply.send(Ok(()));
                }
                Err(message) => {
                    let _ = reply.send(Err(message));
                }
            }
            Ok(QrAction::Stay)
        }
        UiCommand::Password { reply, .. } => {
            let _ = reply.send(Err("Сейчас нужен QR, не пароль.".to_owned()));
            Ok(QrAction::Stay)
        }
    }
}

async fn hold_ready(
    state: Arc<AppState>,
    desk: &Desk,
    inbox: &mut mpsc::UnboundedReceiver<UiCommand>,
    mut link: Connected,
    session: Arc<KeychainSession<SystemSecretStore>>,
) -> Result<LoopControl, String> {
    let cache_note = if telegram_mcp_core::qr_login::warm_dialogs(&link.client)
        .await
        .is_err()
    {
        Some("Кэш чатов заполнен не полностью.".to_owned())
    } else {
        None
    };
    let account_name = link
        .client
        .get_me()
        .await
        .ok()
        .map(|user| user.full_name())
        .filter(|name| !name.is_empty());
    state.set_telegram(Some(Arc::new(LiveTelegram::new(link.client.clone()))));
    let incoming = Arc::clone(&state);
    let task = telegram_mcp_core::live::spawn_incoming_watch(
        link.client.clone(),
        link.take_updates(),
        Arc::new(move |message| incoming.record_incoming(message)),
    );
    state.set_update_task(task);
    let mut phase = Phase::screen("ready");
    phase.account_name = account_name;
    phase.message = cache_note;
    desk.set_phase(phase);

    while let Some(command) = inbox.recv().await {
        match command {
            UiCommand::Logout { reply } => {
                let _ = link.client.sign_out().await;
                end_session(&state, link, session).await;
                let _ = reply.send(Ok(()));
                desk.set_phase(Phase::screen("credentials"));
                return Ok(LoopControl::Restart);
            }
            UiCommand::Save {
                api_id,
                api_hash,
                replace,
                reply,
            } => {
                if replace {
                    let _ = link.client.sign_out().await;
                    end_session(&state, link, session).await;
                    let result = store_login(api_id, api_hash).map(|_| ());
                    let _ = reply.send(result);
                    desk.set_phase(Phase::screen("credentials"));
                    return Ok(LoopControl::Restart);
                }
                let _ = reply.send(store_login(api_id, api_hash).map(|_| ()));
            }
            UiCommand::Password { reply, .. } => {
                let _ = reply.send(Err("Вход уже выполнен.".to_owned()));
            }
        }
    }
    Ok(LoopControl::Continue)
}

async fn end_session(
    state: &AppState,
    link: Connected,
    session: Arc<KeychainSession<SystemSecretStore>>,
) {
    state.set_telegram(None);
    link.shutdown().await;
    match Arc::try_unwrap(session) {
        Ok(session) => {
            let _ = session.wipe().await;
        }
        Err(_) => {
            let _ = wipe_secrets(&SystemSecretStore);
        }
    }
}

async fn wipe_closed_session() -> Result<(), String> {
    let path = session_file_path().map_err(|_| "не найден каталог приложения".to_owned())?;
    if path.exists() {
        if let Ok(session) = KeychainSession::open(&path, SystemSecretStore).await {
            return session
                .wipe()
                .await
                .map_err(|_| "не удалось стереть сессию".to_owned());
        }
    }
    wipe_secrets(&SystemSecretStore).map_err(|_| "не удалось стереть сессию".to_owned())
}
