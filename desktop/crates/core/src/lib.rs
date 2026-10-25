//! Telegram client, QR login, and session storage.

pub mod chats;
pub mod live;
#[cfg(target_os = "macos")]
mod macos_keychain;
pub mod qr_login;
pub mod secrets;
pub mod secured_session;
