//! Console QR login against Telegram's raw auth methods.
//!
//! grammers 0.11 has no QR helper. This follows `auth.exportLoginToken`,
//! waits for `updateLoginToken`, and imports the token on another datacenter
//! when Telegram asks for that.

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use grammers_client::client::PasswordToken;
use grammers_client::sender::{
    ConnectionParams, SenderPool, SenderPoolFatHandle, SenderPoolHandle,
};
use grammers_client::tl;
use grammers_client::{Client, SignInError};
use grammers_session::storages::SqliteSession;
use grammers_session::updates::UpdatesLike;
use grammers_session::Session;
use qrcode::render::unicode::Dense1x2;
use qrcode::QrCode;
use tokio::sync::mpsc;

const DEVICE_MODEL: &str = "Telegram MCP";
const QR_REFRESH_SECS: u64 = 30;

/// Failure of the console QR login. The text never includes credentials.
#[derive(Debug)]
pub struct QrLoginError(String);

impl QrLoginError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for QrLoginError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for QrLoginError {}

impl From<grammers_client::InvocationError> for QrLoginError {
    fn from(error: grammers_client::InvocationError) -> Self {
        Self::new(error.to_string())
    }
}

impl From<SignInError> for QrLoginError {
    fn from(error: SignInError) -> Self {
        Self::new(error.to_string())
    }
}

impl From<io::Error> for QrLoginError {
    fn from(error: io::Error) -> Self {
        Self::new(error.to_string())
    }
}

/// `tg://login?token=` link encoded as base64url without padding.
pub fn login_qr_url(token: &[u8]) -> String {
    format!("tg://login?token={}", base64url(token))
}

/// How long to keep the current QR before exporting a new token.
///
/// Telegram's expiry is usually 30 seconds. A clock that is ahead of the server
/// must not spin the export loop.
pub fn refresh_wait(expires_unix: i32) -> Duration {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    refresh_wait_at(expires_unix, now)
}

fn refresh_wait_at(expires_unix: i32, now_unix: u64) -> Duration {
    let left = i64::from(expires_unix).saturating_sub(i64::try_from(now_unix).unwrap_or(i64::MAX));
    if left <= 0 {
        Duration::from_secs(1)
    } else {
        Duration::from_secs(
            u64::try_from(left)
                .unwrap_or(QR_REFRESH_SECS)
                .min(QR_REFRESH_SECS),
        )
    }
}

/// Sign in, or reuse the saved session, then print the account name and dialog count.
pub async fn run(api_id: i32, api_hash: &str) -> Result<(), QrLoginError> {
    let session_path = session_path();
    println!("Файл сессии: {}", session_path.display());

    let session = Arc::new(
        SqliteSession::open(&session_path)
            .await
            .map_err(|error| QrLoginError::new(error.to_string()))?,
    );
    restrict_session_file(&session_path)?;

    let params = ConnectionParams {
        device_model: DEVICE_MODEL.to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        ..ConnectionParams::default()
    };
    let SenderPool {
        runner,
        handle,
        mut updates,
    } = SenderPool::with_configuration(Arc::clone(&session), api_id, params);
    let client = Client::new(handle.clone());
    let runner_task = tokio::spawn(runner.run());

    let result = async {
        if client.is_authorized().await? {
            println!("Сессия уже есть, QR не нужен.");
        } else {
            sign_in_with_qr(
                &client,
                session.as_ref(),
                &handle,
                &mut updates,
                api_id,
                api_hash,
            )
            .await?;
        }
        print_summary(&client).await
    }
    .await;

    handle.quit();
    let _ = runner_task.await;
    result
}

fn session_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("qr-spike.session")
}

fn restrict_session_file(path: &std::path::Path) -> Result<(), QrLoginError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

async fn sign_in_with_qr<S>(
    client: &Client,
    session: &S,
    handle: &SenderPoolHandle,
    updates: &mut mpsc::Receiver<UpdatesLike>,
    api_id: i32,
    api_hash: &str,
) -> Result<(), QrLoginError>
where
    S: Session,
    S::Error: std::fmt::Display,
{
    println!("Отсканируйте QR в Telegram: Настройки → Устройства → Подключить устройство.");
    println!("Новый код появится сам, примерно каждые 30 секунд.");

    loop {
        match advance_login(client, session, handle, api_id, api_hash).await? {
            LoginStep::Authorized => return Ok(()),
            LoginStep::PasswordNeeded => {
                finish_password(client).await?;
                return Ok(());
            }
            LoginStep::Qr { token, expires } => {
                println!();
                println!("{}", render_qr(&login_qr_url(&token))?);
                println!();
                let _confirmed = wait_for_login_update(updates, refresh_wait(expires)).await;
            }
        }
    }
}

enum Accepted {
    Authorized,
    Qr { token: Vec<u8>, expires: i32 },
    PasswordNeeded,
}

/// One step of QR login. The caller shows the code or asks for the cloud password.
pub enum LoginStep {
    Authorized,
    Qr { token: Vec<u8>, expires: i32 },
    PasswordNeeded,
}

pub enum PasswordAttempt {
    Accepted,
    Rejected(PasswordToken),
}

/// A connected client. Dropping it does not log out.
pub struct Connected {
    pub client: Client,
    pub handle: SenderPoolFatHandle,
    updates: Option<mpsc::Receiver<UpdatesLike>>,
    runner: tokio::task::JoinHandle<()>,
}

impl Connected {
    pub fn updates_mut(&mut self) -> &mut mpsc::Receiver<UpdatesLike> {
        self.updates
            .as_mut()
            .expect("updates receiver already taken")
    }

    pub fn take_updates(&mut self) -> mpsc::Receiver<UpdatesLike> {
        self.updates.take().expect("updates receiver already taken")
    }
}

impl Connected {
    pub async fn shutdown(self) {
        drop(self);
    }
}

impl Drop for Connected {
    fn drop(&mut self) {
        self.handle.quit();
        self.runner.abort();
    }
}

pub async fn connect<S>(session: Arc<S>, api_id: i32) -> Result<Connected, QrLoginError>
where
    S: Session,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let params = ConnectionParams {
        device_model: DEVICE_MODEL.to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        ..ConnectionParams::default()
    };
    let SenderPool {
        runner,
        handle,
        updates,
    } = SenderPool::with_configuration(session, api_id, params);
    let client = Client::new(handle.clone());
    let runner = tokio::spawn(runner.run());
    Ok(Connected {
        client,
        handle,
        updates: Some(updates),
        runner,
    })
}

pub async fn advance_login<S>(
    client: &Client,
    session: &S,
    handle: &SenderPoolHandle,
    api_id: i32,
    api_hash: &str,
) -> Result<LoginStep, QrLoginError>
where
    S: Session,
    S::Error: std::fmt::Display,
{
    let exported = match client
        .invoke(&tl::functions::auth::ExportLoginToken {
            api_id,
            api_hash: api_hash.to_string(),
            except_ids: Vec::new(),
        })
        .await
    {
        Ok(token) => token,
        Err(error) if error.is("SESSION_PASSWORD_NEEDED") => {
            return Ok(LoginStep::PasswordNeeded);
        }
        Err(error) => return Err(error.into()),
    };
    match accept_token(client, session, handle, exported).await? {
        Accepted::Authorized => Ok(LoginStep::Authorized),
        Accepted::PasswordNeeded => Ok(LoginStep::PasswordNeeded),
        Accepted::Qr { token, expires } => Ok(LoginStep::Qr { token, expires }),
    }
}

pub async fn begin_password(client: &Client) -> Result<PasswordToken, QrLoginError> {
    let info: tl::types::account::Password = client
        .invoke(&tl::functions::account::GetPassword {})
        .await?
        .into();
    Ok(PasswordToken::new(info))
}

pub async fn try_password(
    client: &Client,
    token: PasswordToken,
    password: &str,
) -> Result<PasswordAttempt, QrLoginError> {
    match client.check_password(token, password.to_owned()).await {
        Ok(_) => Ok(PasswordAttempt::Accepted),
        Err(SignInError::InvalidPassword(token)) => Ok(PasswordAttempt::Rejected(token)),
        Err(error) => Err(error.into()),
    }
}

pub fn login_qr_svg(token: &[u8]) -> Result<String, QrLoginError> {
    let code =
        QrCode::new(login_qr_url(token)).map_err(|error| QrLoginError::new(error.to_string()))?;
    let scale = 8usize;
    let quiet = 4usize;
    let modules = code.width();
    let size = (modules + quiet * 2) * scale;
    let mut rects = String::new();
    for y in 0..modules {
        for x in 0..modules {
            if code[(x, y)] != qrcode::Color::Dark {
                continue;
            }
            let left = (x + quiet) * scale;
            let top = (y + quiet) * scale;
            rects.push_str(&format!(
                r#"<rect x="{left}" y="{top}" width="{scale}" height="{scale}"/>"#
            ));
        }
    }
    Ok(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {size} {size}\" shape-rendering=\"crispEdges\"><rect width=\"100%\" height=\"100%\" fill=\"#fff\"/>{rects}</svg>"
    ))
}

/// One pass over dialogs so later calls can reuse the peer cache.
pub async fn warm_dialogs(client: &Client) -> Result<(), QrLoginError> {
    let mut dialogs = client.iter_dialogs();
    while dialogs.next().await?.is_some() {}
    Ok(())
}

async fn accept_token<S>(
    client: &Client,
    session: &S,
    handle: &SenderPoolHandle,
    mut token: tl::enums::auth::LoginToken,
) -> Result<Accepted, QrLoginError>
where
    S: Session,
    S::Error: std::fmt::Display,
{
    for _ in 0..4 {
        match token {
            tl::enums::auth::LoginToken::Token(qr) => {
                return Ok(Accepted::Qr {
                    token: qr.token,
                    expires: qr.expires,
                });
            }
            tl::enums::auth::LoginToken::Success(_) => return Ok(Accepted::Authorized),
            tl::enums::auth::LoginToken::MigrateTo(migrate) => {
                match import_migrated(client, session, handle, migrate).await? {
                    ImportOutcome::Next(next) => token = *next,
                    ImportOutcome::PasswordNeeded => return Ok(Accepted::PasswordNeeded),
                }
            }
        }
    }
    Err(QrLoginError::new(
        "слишком много переходов между дата-центрами",
    ))
}

enum ImportOutcome {
    Next(Box<tl::enums::auth::LoginToken>),
    PasswordNeeded,
}

async fn import_migrated<S>(
    client: &Client,
    session: &S,
    handle: &SenderPoolHandle,
    migrate: tl::types::auth::LoginTokenMigrateTo,
) -> Result<ImportOutcome, QrLoginError>
where
    S: Session,
    S::Error: std::fmt::Display,
{
    let old_dc = session
        .home_dc_id()
        .map_err(|error| QrLoginError::new(error.to_string()))?;
    let imported = client
        .invoke_in_dc(
            migrate.dc_id,
            &tl::functions::auth::ImportLoginToken {
                token: migrate.token,
            },
        )
        .await;

    let switch_home = match &imported {
        Ok(_) => true,
        Err(error) if error.is("SESSION_PASSWORD_NEEDED") => true,
        Err(_) => false,
    };
    if switch_home && migrate.dc_id != old_dc {
        session
            .set_home_dc_id(migrate.dc_id)
            .await
            .map_err(|error| QrLoginError::new(error.to_string()))?;
        handle.disconnect_from_dc(old_dc);
    }

    match imported {
        Ok(token) => Ok(ImportOutcome::Next(Box::new(token))),
        Err(error) if error.is("SESSION_PASSWORD_NEEDED") => Ok(ImportOutcome::PasswordNeeded),
        Err(error) => Err(error.into()),
    }
}

async fn finish_password(client: &Client) -> Result<(), QrLoginError> {
    let mut token = begin_password(client).await?;
    loop {
        let password = prompt_password(token.hint())?;
        match try_password(client, token, &password).await? {
            PasswordAttempt::Accepted => return Ok(()),
            PasswordAttempt::Rejected(again) => {
                eprintln!("Неверный пароль.");
                token = again;
            }
        }
    }
}

fn prompt_password(hint: Option<&str>) -> Result<String, QrLoginError> {
    println!(
        "Включен облачный пароль. Подсказка: {}",
        hint.unwrap_or("нет")
    );
    print!("Пароль (символы будут видны): ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let password = line.trim_end_matches(['\r', '\n']).to_string();
    if password.is_empty() {
        return Err(QrLoginError::new("вход отменен"));
    }
    Ok(password)
}

pub async fn wait_for_login_update(
    updates: &mut mpsc::Receiver<UpdatesLike>,
    delay: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + delay;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match tokio::time::timeout(remaining, updates.recv()).await {
            Ok(Some(update)) if contains_login_token(&update) => return true,
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return false,
        }
    }
}

fn contains_login_token(update: &UpdatesLike) -> bool {
    let UpdatesLike::Updates(updates) = update else {
        return false;
    };
    match updates {
        tl::enums::Updates::UpdateShort(short) => is_login_token(&short.update),
        tl::enums::Updates::Updates(batch) => batch.updates.iter().any(is_login_token),
        tl::enums::Updates::Combined(batch) => batch.updates.iter().any(is_login_token),
        _ => false,
    }
}

fn is_login_token(update: &tl::enums::Update) -> bool {
    matches!(update, tl::enums::Update::LoginToken)
}

fn render_qr(contents: &str) -> Result<String, QrLoginError> {
    let code = QrCode::new(contents).map_err(|error| QrLoginError::new(error.to_string()))?;
    Ok(code.render::<Dense1x2>().quiet_zone(true).build())
}

async fn print_summary(client: &Client) -> Result<(), QrLoginError> {
    let me = client.get_me().await?;
    let name = me.full_name();
    let name = if name.is_empty() {
        "без имени".to_string()
    } else {
        name
    };
    let count = client.iter_dialogs().total().await?;
    println!("Аккаунт: {name}");
    println!("Диалогов: {count}");
    Ok(())
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
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
    let rest = bytes.len() - index;
    if rest == 1 {
        let chunk = u32::from(bytes[index]) << 16;
        output.push(ALPHABET[((chunk >> 18) & 0x3f) as usize] as char);
        output.push(ALPHABET[((chunk >> 12) & 0x3f) as usize] as char);
    } else if rest == 2 {
        let chunk = u32::from(bytes[index]) << 16 | u32::from(bytes[index + 1]) << 8;
        output.push(ALPHABET[((chunk >> 18) & 0x3f) as usize] as char);
        output.push(ALPHABET[((chunk >> 12) & 0x3f) as usize] as char);
        output.push(ALPHABET[((chunk >> 6) & 0x3f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_url_is_unpadded_base64url() {
        assert_eq!(login_qr_url(b"hello"), "tg://login?token=aGVsbG8");
        assert_eq!(login_qr_url(&[0xfb, 0xff, 0xfe]), "tg://login?token=-__-");
        let token = login_qr_url(&[0xfb, 0x00, 0xff])
            .split_once("token=")
            .unwrap()
            .1
            .to_string();
        assert!(!token.contains('+'));
        assert!(!token.contains('/'));
        assert!(!token.contains('='));
    }

    #[test]
    fn qr_svg_is_markup_without_the_login_url() {
        let svg = login_qr_svg(&[0xfb, 0xff, 0xfe]).unwrap();
        assert!(svg.starts_with("<svg "));
        assert!(svg.contains("<rect "));
        assert!(!svg.contains("tg://"));
    }

    #[test]
    fn refresh_wait_stays_within_thirty_seconds() {
        let now = 1_700_000_000;
        assert_eq!(refresh_wait_at(now + 5, now as u64), Duration::from_secs(5));
        assert_eq!(
            refresh_wait_at(now + 3_600, now as u64),
            Duration::from_secs(30)
        );
        assert_eq!(
            refresh_wait_at(now - 10, now as u64),
            Duration::from_secs(1)
        );
    }
}
