//! Local paths for sending and downloading Telegram files.
//!
//! A caller-supplied path must be absolute. Downloads without a path go to the
//! app data `downloads` directory.

use std::path::{Component, Path, PathBuf};

use telegram_mcp_ipc::app_data_dir;

pub const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;

pub fn readable_file(raw: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(raw.trim());
    if !path.is_absolute() || path.components().any(|part| matches!(part, Component::ParentDir)) {
        return Err("нужен абсолютный путь к файлу".to_owned());
    }
    let path = path.canonicalize().map_err(|_| "файл не найден".to_owned())?;
    if !path.is_file() {
        return Err("это не файл".to_owned());
    }
    let len = std::fs::metadata(&path)
        .map_err(|_| "файл не найден".to_owned())?
        .len();
    if len == 0 {
        return Err("файл пустой".to_owned());
    }
    if len > MAX_FILE_BYTES {
        return Err(format!("файл больше лимита {MAX_FILE_BYTES} байт"));
    }
    Ok(path)
}

pub fn writable_file(requested: Option<&str>, default_name: &str) -> Result<PathBuf, String> {
    if let Some(raw) = requested {
        let path = PathBuf::from(raw.trim());
        if !path.is_absolute() || path.components().any(|part| matches!(part, Component::ParentDir))
        {
            return Err("нужен абсолютный путь к файлу".to_owned());
        }
        let parent = path
            .parent()
            .filter(|dir| dir.is_dir())
            .ok_or_else(|| "каталог для файла не найден".to_owned())?;
        let _ = parent;
        if path.file_name().is_none() {
            return Err("не указано имя файла".to_owned());
        }
        return Ok(path);
    }
    let dir = downloads_dir()?;
    std::fs::create_dir_all(&dir).map_err(|_| "не удалось создать каталог загрузок".to_owned())?;
    Ok(dir.join(safe_name(default_name)))
}

pub fn downloads_dir() -> Result<PathBuf, String> {
    let dir = app_data_dir()
        .map_err(|_| "каталог данных приложения недоступен".to_owned())?
        .join("downloads");
    Ok(dir)
}

pub fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub fn mime_for(path: &Path) -> String {
    match extension(path).as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "ogg" | "opus" => "audio/ogg",
        "mp3" => "audio/mpeg",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
    .to_owned()
}

/// How `send_file` should upload a path when the caller did not force a kind.
pub fn upload_kind(path: &Path) -> &'static str {
    match extension(path).as_str() {
        "jpg" | "jpeg" | "png" | "webp" => "photo",
        "mp4" | "mov" | "mkv" | "webm" => "video",
        _ => "document",
    }
}

fn safe_name(name: &str) -> String {
    let name = Path::new(name)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("telegram.bin");
    if name.is_empty() || name.contains("..") {
        "telegram.bin".to_owned()
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_is_rejected() {
        assert!(readable_file("note.txt").is_err());
        assert!(writable_file(Some("note.txt"), "a.bin").is_err());
    }

    #[test]
    fn photos_and_videos_are_not_plain_documents() {
        assert_eq!(upload_kind(Path::new("/tmp/a.JPG")), "photo");
        assert_eq!(upload_kind(Path::new("/tmp/a.mp4")), "video");
        assert_eq!(upload_kind(Path::new("/tmp/a.pdf")), "document");
    }
}
