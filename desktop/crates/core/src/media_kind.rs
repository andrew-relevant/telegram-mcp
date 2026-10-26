//! Classify Telegram media. A link preview is not an attachment.

use grammers_client::media::{Document, Media, Sticker};
use grammers_client::tl;

use crate::chats::MediaInfo;

pub(crate) fn describe(media: &Media) -> Option<MediaInfo> {
    match media {
        Media::WebPage(_) => None,
        Media::Photo(photo) => Some(info(
            "photo",
            None,
            Some("image/jpeg"),
            photo.size(),
            None,
            None,
        )),
        Media::Sticker(sticker) => Some(sticker_info(sticker)),
        Media::Document(document) => Some(document_info(document)),
        Media::Contact(_) => Some(info("contact", None, None, None, None, None)),
        Media::Poll(_) => Some(info("poll", None, None, None, None, None)),
        Media::Geo(_) | Media::GeoLive(_) | Media::Venue(_) => {
            Some(info("geo", None, None, None, None, None))
        }
        Media::Dice(dice) => Some(info("dice", None, None, None, None, Some(dice.raw.emoticon.as_str()))),
        _ => Some(info("media", None, None, None, None, None)),
    }
}

fn sticker_info(sticker: &Sticker) -> MediaInfo {
    let mut info = document_info(&sticker.document);
    info.kind = "sticker".to_owned();
    info.emoji = Some(sticker.emoji().to_owned());
    info
}

fn document_info(document: &Document) -> MediaInfo {
    let mut kind = "document";
    let mut duration = None;
    if let Some(tl::enums::Document::Document(raw)) = document.raw.document.as_ref() {
        let mut voice = false;
        let mut audio = false;
        let mut video = false;
        let mut round = false;
        let mut animated = false;
        for attribute in &raw.attributes {
            match attribute {
                tl::enums::DocumentAttribute::Audio(audio_attr) => {
                    audio = true;
                    voice = audio_attr.voice;
                    duration = Some(f64::from(audio_attr.duration));
                }
                tl::enums::DocumentAttribute::Video(video_attr) => {
                    video = true;
                    round = video_attr.round_message;
                    duration = Some(video_attr.duration);
                }
                tl::enums::DocumentAttribute::Animated => animated = true,
                _ => {}
            }
        }
        kind = if voice {
            "voice"
        } else if round {
            "video_note"
        } else if video {
            "video"
        } else if audio {
            "audio"
        } else if animated {
            "gif"
        } else {
            "document"
        };
    }
    info(
        kind,
        document.name().map(str::to_owned),
        document.mime_type(),
        document.size(),
        duration,
        None,
    )
}

fn info(
    kind: &str,
    file_name: Option<String>,
    mime: Option<&str>,
    size: Option<usize>,
    duration: Option<f64>,
    emoji: Option<&str>,
) -> MediaInfo {
    MediaInfo {
        kind: kind.to_owned(),
        file_name,
        mime: mime.map(str::to_owned),
        size: size.and_then(|size| u64::try_from(size).ok()),
        duration,
        emoji: emoji.map(str::to_owned),
    }
}
