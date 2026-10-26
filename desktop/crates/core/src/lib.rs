//! Telegram client, QR login, and session storage.

pub mod chats;
pub mod files;
pub mod live;
mod media_kind;
mod ops;
pub mod qr_login;
pub mod secrets;
#[cfg(target_os = "macos")]
mod secret_file;
pub mod secured_session;
