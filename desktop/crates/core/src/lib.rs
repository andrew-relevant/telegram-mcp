//! Telegram client, QR login, and session storage.

pub mod chats;
pub mod files;
pub mod live;
mod media_kind;
mod ops;
#[cfg(target_os = "macos")]
mod macos_keychain;
pub mod qr_login;
pub mod secrets;
pub mod secured_session;
