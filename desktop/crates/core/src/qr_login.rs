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
use grammers_client::sender::{ConnectionParams, SenderPool, SenderPoolHandle};
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
            sign_in_with_qr(&client, &session, &handle, &mut updates, api_id, api_hash).await?;
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

async fn sign_in_with_qr(
    client: &Client,
    session: &SqliteSession,
    handle: &SenderPoolHandle,
    updates: &mut mpsc::Receiver<UpdatesLike>,
    api_id: i32,
    api_hash: &str,
) -> Result<(), QrLoginError> {
    println!("Отсканируйте QR в Telegram: Настройки → Устройства → Подключить устройство.");
    println!("Новый код появится сам, примерно каждые 30 секунд.");

    loop {
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
                finish_password(client).await?;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };

        match accept_token(client, session, handle, exported).await? {
            Accepted::Authorized => return Ok(()),
            Accepted::Qr { token, expires } => {
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
}

async fn accept_token(
    client: &Client,
    session: &SqliteSession,
    handle: &SenderPoolHandle,
    mut token: tl::enums::auth::LoginToken,
) -> Result<Accepted, QrLoginError> {
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
                    Some(next) => token = next,
                    None => return Ok(Accepted::Authorized),
                }
            }
        }
    }
    Err(QrLoginError::new(
        "слишком много переходов между дата-центрами",
    ))
}

async fn import_migrated(
    client: &Client,
    session: &SqliteSession,
    handle: &SenderPoolHandle,
    migrate: tl::types::auth::LoginTokenMigrateTo,
) -> Result<Option<tl::enums::auth::LoginToken>, QrLoginError> {
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
        Ok(token) => Ok(Some(token)),
        Err(error) if error.is("SESSION_PASSWORD_NEEDED") => {
            finish_password(client).await?;
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

async fn finish_password(client: &Client) -> Result<(), QrLoginError> {
    let info: tl::types::account::Password = client
        .invoke(&tl::functions::account::GetPassword {})
        .await?
        .into();
    let mut token = PasswordToken::new(info);
    loop {
        let password = prompt_password(token.hint())?;
        match client.check_password(token, password).await {
            Ok(_) => return Ok(()),
            Err(SignInError::InvalidPassword(again)) => {
                eprintln!("Неверный пароль.");
                token = again;
            }
            Err(error) => return Err(error.into()),
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

async fn wait_for_login_update(updates: &mut mpsc::Receiver<UpdatesLike>, delay: Duration) -> bool {
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
