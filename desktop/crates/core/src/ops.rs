//! Chat actions that the high-level client does not expose directly:
//! forum topics, typed uploads, and member changes.

use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use grammers_client::media::{Downloadable, InputAudio, InputVideo};
use grammers_client::message::InputMessage;
use grammers_client::peer::Role;
use grammers_client::session::types::{PeerId, PeerKind, PeerRef};
use grammers_client::tl;
use grammers_client::Client;
use jiff::Timestamp;

use crate::chats::{
    invocation_message, MediaInfo, MessageRecord, ParticipantRecord, TopicRecord,
};
use crate::files::{self, MAX_FILE_BYTES};
use crate::media_kind;

pub(crate) fn message_record(message: &grammers_client::message::Message) -> MessageRecord {
    let peer = message.peer();
    let (reply_to, topic_id) = message
        .reply_header()
        .as_ref()
        .map(reply_parts)
        .unwrap_or((None, None));
    MessageRecord {
        id: message.id(),
        chat_id: peer.and_then(peer_id).unwrap_or(0),
        chat_title: peer.map(peer_title).unwrap_or_default(),
        sender: message
            .sender()
            .map(peer_title)
            .filter(|name| !name.is_empty())
            .or_else(|| message.post_author().map(str::to_owned))
            .unwrap_or_default(),
        text: message.text().to_owned(),
        date: message.date().to_string(),
        media: message.media().as_ref().and_then(media_kind::describe),
        reply_to,
        topic_id,
        grouped_id: message.grouped_id(),
    }
}

pub(crate) async fn download_media(
    client: &Client,
    peer: PeerRef,
    message_id: i32,
    file_path: Option<&str>,
) -> Result<String, String> {
    let messages = client
        .get_messages_by_id(peer, &[message_id])
        .await
        .map_err(invocation_message)?;
    let message = messages
        .into_iter()
        .flatten()
        .next()
        .ok_or_else(|| "сообщение не найдено".to_owned())?;
    let media = message
        .media()
        .filter(|media| media_kind::describe(media).is_some())
        .ok_or_else(|| "во вложении нет файла".to_owned())?;
    if let Some(size) = Downloadable::size(&media) {
        if u64::try_from(size).unwrap_or(u64::MAX) > MAX_FILE_BYTES {
            return Err(format!("файл больше лимита {MAX_FILE_BYTES} байт"));
        }
    }
    let name = download_name(message_id, media_kind::describe(&media).as_ref());
    let path = files::writable_file(file_path, &name)?;
    client
        .download_media(&media, &path)
        .await
        .map_err(invocation_message)?;
    let len = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
    if len > MAX_FILE_BYTES {
        let _ = std::fs::remove_file(&path);
        return Err(format!("файл больше лимита {MAX_FILE_BYTES} байт"));
    }
    Ok(path.display().to_string())
}

pub(crate) async fn send_path(
    client: &Client,
    peer: PeerRef,
    path: &Path,
    caption: &str,
    topic_id: Option<i32>,
    kind: &str,
    emoji: Option<&str>,
) -> Result<(), String> {
    let uploaded = client
        .upload_file(path)
        .await
        .map_err(|_| "не удалось загрузить файл".to_owned())?;
    let mime = files::mime_for(path);
    let mut input = InputMessage::new()
        .text(caption.to_owned())
        .reply_to(topic_id);
    input = match kind {
        "photo" => input.photo(uploaded),
        "video" => input.media(InputVideo::new(uploaded).mime_type(mime)),
        "voice" => input.media(InputAudio::new(uploaded).mime_type(mime).voice(true)),
        "sticker" => input.media(sticker_media(uploaded, path, emoji.unwrap_or("🙂"))),
        _ => input.document(uploaded),
    };
    client
        .send_message(peer, input)
        .await
        .map_err(invocation_message)?;
    Ok(())
}

pub(crate) async fn history_topic(
    client: &Client,
    peer: PeerRef,
    chat_id: i64,
    chat_title: &str,
    topic_id: i32,
    limit: usize,
) -> Result<Vec<MessageRecord>, String> {
    let result = client
        .invoke(&tl::functions::messages::GetReplies {
            peer: peer.into(),
            msg_id: topic_id,
            offset_id: 0,
            offset_date: 0,
            add_offset: 0,
            limit: i32::try_from(limit).unwrap_or(100),
            max_id: 0,
            min_id: 0,
            hash: 0,
        })
        .await
        .map_err(topic_error)?;
    Ok(records_from_messages(result, chat_id, chat_title))
}

pub(crate) async fn search_topic(
    client: &Client,
    peer: PeerRef,
    chat_id: i64,
    chat_title: &str,
    query: &str,
    topic_id: i32,
    limit: usize,
) -> Result<Vec<MessageRecord>, String> {
    let result = client
        .invoke(&tl::functions::messages::Search {
            peer: peer.into(),
            q: query.to_owned(),
            from_id: None,
            saved_peer_id: None,
            saved_reaction: None,
            top_msg_id: Some(topic_id),
            filter: tl::enums::MessagesFilter::InputMessagesFilterEmpty,
            min_date: 0,
            max_date: 0,
            offset_id: 0,
            add_offset: 0,
            limit: i32::try_from(limit).unwrap_or(100),
            max_id: 0,
            min_id: 0,
            hash: 0,
        })
        .await
        .map_err(topic_error)?;
    Ok(records_from_messages(result, chat_id, chat_title))
}

pub(crate) async fn pinned(
    client: &Client,
    peer: PeerRef,
) -> Result<Vec<MessageRecord>, String> {
    let mut iter = client
        .search_messages(peer)
        .filter(tl::enums::MessagesFilter::InputMessagesFilterPinned)
        .limit(100);
    let mut messages = Vec::new();
    while let Some(message) = iter.next().await.map_err(invocation_message)? {
        messages.push(message_record(&message));
    }
    Ok(messages)
}

pub(crate) async fn forward_to_topic(
    client: &Client,
    source: PeerRef,
    destination: PeerRef,
    message_id: i32,
    topic_id: i32,
) -> Result<(), String> {
    client
        .invoke(&tl::functions::messages::ForwardMessages {
            silent: false,
            background: false,
            with_my_score: false,
            drop_author: false,
            drop_media_captions: false,
            noforwards: false,
            allow_paid_floodskip: false,
            from_ephemeral: false,
            from_peer: source.into(),
            id: vec![message_id],
            random_id: vec![random_id()],
            to_peer: destination.into(),
            top_msg_id: Some(topic_id),
            reply_to: None,
            schedule_date: None,
            schedule_repeat_period: None,
            send_as: None,
            quick_reply_shortcut: None,
            effect: None,
            video_timestamp: None,
            allow_paid_stars: None,
            suggested_post: None,
        })
        .await
        .map_err(invocation_message)?;
    Ok(())
}

pub(crate) async fn participants(
    client: &Client,
    peer: PeerRef,
    limit: usize,
) -> Result<Vec<ParticipantRecord>, String> {
    let mut iter = client.iter_participants(peer);
    let mut people = Vec::new();
    while people.len() < limit {
        let Some(participant) = iter.next().await.map_err(invocation_message)? else {
            break;
        };
        let id = participant
            .user
            .id()
            .bot_api_dialog_id()
            .unwrap_or_else(|| participant.user.id().bot_api_dialog_id_unchecked());
        people.push(ParticipantRecord {
            id,
            name: participant.user.full_name(),
            username: participant.user.username().map(str::to_owned),
            role: role_name(&participant.role).to_owned(),
        });
    }
    Ok(people)
}

pub(crate) async fn invite(
    client: &Client,
    chat: PeerRef,
    user: PeerRef,
) -> Result<(), String> {
    if user.id.kind() != PeerKind::User {
        return Err("приглашать можно только пользователя".to_owned());
    }
    let invited = if chat.id.kind() == PeerKind::Channel {
        client
            .invoke(&tl::functions::channels::InviteToChannel {
                channel: chat.into(),
                users: vec![user.into()],
            })
            .await
            .map_err(invocation_message)?
    } else if chat.id.kind() == PeerKind::Chat {
        client
            .invoke(&tl::functions::messages::AddChatUser {
                chat_id: chat.into(),
                user_id: user.into(),
                fwd_limit: 0,
            })
            .await
            .map_err(invocation_message)?
    } else {
        return Err("в личный чат нельзя пригласить участника".to_owned());
    };
    let tl::enums::messages::InvitedUsers::Users(invited) = invited;
    if invited.missing_invitees.is_empty() {
        Ok(())
    } else {
        Err("пользователь не добавлен".to_owned())
    }
}

pub(crate) async fn create_group(
    client: &Client,
    title: &str,
    users: &[PeerRef],
) -> Result<String, String> {
    if users.is_empty() {
        return Err("не указаны участники".to_owned());
    }
    if users.iter().any(|user| user.id.kind() != PeerKind::User) {
        return Err("в группу можно добавить только пользователей".to_owned());
    }
    let invited = client
        .invoke(&tl::functions::messages::CreateChat {
            users: users.iter().copied().map(Into::into).collect(),
            title: title.to_owned(),
            ttl_period: None,
        })
        .await
        .map_err(invocation_message)?;
    let tl::enums::messages::InvitedUsers::Users(invited) = invited;
    Ok(created_text("Группа создана", &invited.updates))
}

pub(crate) async fn create_channel(
    client: &Client,
    title: &str,
    about: &str,
    megagroup: bool,
    forum: bool,
) -> Result<String, String> {
    let updates = client
        .invoke(&tl::functions::channels::CreateChannel {
            broadcast: !megagroup,
            megagroup,
            for_import: false,
            forum: forum && megagroup,
            title: title.to_owned(),
            about: about.to_owned(),
            geo_point: None,
            address: None,
            ttl_period: None,
        })
        .await
        .map_err(invocation_message)?;
    let label = if megagroup { "Группа создана" } else { "Канал создан" };
    Ok(created_text(label, &updates))
}

pub(crate) async fn list_topics(
    client: &Client,
    peer: PeerRef,
    query: Option<&str>,
    offset_topic: i32,
    limit: usize,
) -> Result<Vec<TopicRecord>, String> {
    let result = client
        .invoke(&tl::functions::messages::GetForumTopics {
            peer: peer.into(),
            q: query
                .map(str::trim)
                .filter(|query| !query.is_empty())
                .map(str::to_owned),
            offset_date: 0,
            offset_id: 0,
            offset_topic,
            limit: i32::try_from(limit).unwrap_or(100),
        })
        .await
        .map_err(topic_error)?;
    let tl::enums::messages::ForumTopics::Topics(result) = result;
    let mut topics = Vec::new();
    for topic in result.topics {
        let tl::enums::ForumTopic::Topic(topic) = topic else {
            continue;
        };
        topics.push(TopicRecord {
            id: topic.id,
            title: topic.title,
            unread: topic.unread_count,
            closed: topic.closed,
            hidden: topic.hidden,
        });
    }
    Ok(topics)
}

pub(crate) async fn create_topic(
    client: &Client,
    peer: PeerRef,
    title: &str,
) -> Result<String, String> {
    let updates = client
        .invoke(&tl::functions::messages::CreateForumTopic {
            title_missing: false,
            peer: peer.into(),
            title: title.to_owned(),
            icon_color: None,
            icon_emoji_id: None,
            random_id: random_id(),
            send_as: None,
        })
        .await
        .map_err(topic_error)?;
    match first_message_id(&updates) {
        Some(id) => Ok(format!("Тема создана. topic_id: {id}.")),
        None => Ok("Тема создана.".to_owned()),
    }
}

fn records_from_messages(
    result: tl::enums::messages::Messages,
    chat_id: i64,
    chat_title: &str,
) -> Vec<MessageRecord> {
    let (messages, users, _chats) = unpack_messages(result);
    let names = user_names(&users);
    messages
        .iter()
        .filter_map(|message| record_from_tl(message, &names, chat_id, chat_title))
        .collect()
}

fn record_from_tl(
    message: &tl::enums::Message,
    names: &HashMap<i64, String>,
    chat_id: i64,
    chat_title: &str,
) -> Option<MessageRecord> {
    match message {
        tl::enums::Message::Empty(_) => None,
        tl::enums::Message::Message(message) => {
            let (reply_to, topic_id) = message
                .reply_to
                .as_ref()
                .map(reply_parts)
                .unwrap_or((None, None));
            let sender_id = message.from_id.as_ref().and_then(sender_id);
            Some(MessageRecord {
                id: message.id,
                chat_id,
                chat_title: chat_title.to_owned(),
                sender: sender_id
                    .and_then(|id| names.get(&id).cloned())
                    .or_else(|| message.post_author.clone())
                    .unwrap_or_default(),
                text: message.message.clone(),
                date: format_date(message.date),
                media: message
                    .media
                    .clone()
                    .and_then(grammers_client::media::Media::from_raw)
                    .as_ref()
                    .and_then(media_kind::describe),
                reply_to,
                topic_id,
                grouped_id: message.grouped_id,
            })
        }
        tl::enums::Message::Service(message) => {
            let (reply_to, topic_id) = message
                .reply_to
                .as_ref()
                .map(reply_parts)
                .unwrap_or((None, None));
            let sender_id = message.from_id.as_ref().and_then(sender_id);
            Some(MessageRecord {
                id: message.id,
                chat_id,
                chat_title: chat_title.to_owned(),
                sender: sender_id
                    .and_then(|id| names.get(&id).cloned())
                    .unwrap_or_default(),
                text: String::new(),
                date: format_date(message.date),
                media: None,
                reply_to,
                topic_id,
                grouped_id: None,
            })
        }
    }
}

fn reply_parts(header: &tl::enums::MessageReplyHeader) -> (Option<i32>, Option<i32>) {
    match header {
        tl::enums::MessageReplyHeader::Header(header) => {
            (header.reply_to_msg_id, header.reply_to_top_id)
        }
        tl::enums::MessageReplyHeader::MessageReplyStoryHeader(_) => (None, None),
    }
}

fn sender_id(peer: &tl::enums::Peer) -> Option<i64> {
    match peer {
        tl::enums::Peer::User(user) => Some(user.user_id),
        _ => None,
    }
}

fn user_names(users: &[tl::enums::User]) -> HashMap<i64, String> {
    users
        .iter()
        .filter_map(|user| {
            let tl::enums::User::User(user) = user else {
                return None;
            };
            let mut name = user.first_name.clone().unwrap_or_default();
            if let Some(last) = &user.last_name {
                if !name.is_empty() {
                    name.push(' ');
                }
                name.push_str(last);
            }
            Some((user.id, name))
        })
        .collect()
}

fn unpack_messages(
    result: tl::enums::messages::Messages,
) -> (
    Vec<tl::enums::Message>,
    Vec<tl::enums::User>,
    Vec<tl::enums::Chat>,
) {
    use tl::enums::messages::Messages;
    match result {
        Messages::Messages(messages) => (messages.messages, messages.users, messages.chats),
        Messages::Slice(messages) => (messages.messages, messages.users, messages.chats),
        Messages::ChannelMessages(messages) => {
            (messages.messages, messages.users, messages.chats)
        }
        Messages::NotModified(_) => (Vec::new(), Vec::new(), Vec::new()),
    }
}

fn sticker_media(
    uploaded: grammers_client::media::Uploaded,
    path: &Path,
    emoji: &str,
) -> tl::enums::InputMedia {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("sticker.webp")
        .to_owned();
    tl::types::InputMediaUploadedDocument {
        nosound_video: false,
        force_file: false,
        spoiler: false,
        file: uploaded.raw,
        thumb: None,
        mime_type: files::mime_for(path),
        attributes: vec![
            tl::types::DocumentAttributeFilename { file_name }.into(),
            tl::types::DocumentAttributeSticker {
                mask: false,
                alt: emoji.to_owned(),
                stickerset: tl::enums::InputStickerSet::Empty,
                mask_coords: None,
            }
            .into(),
        ],
        stickers: None,
        video_cover: None,
        video_timestamp: None,
        ttl_seconds: None,
    }
    .into()
}

fn download_name(message_id: i32, media: Option<&MediaInfo>) -> String {
    let kind = media.map(|media| media.kind.as_str()).unwrap_or("file");
    if let Some(name) = media.and_then(|media| media.file_name.as_deref()) {
        if let Some(name) = Path::new(name).file_name().and_then(|name| name.to_str()) {
            if !name.is_empty() && !name.contains("..") {
                return format!("{message_id}_{name}");
            }
        }
    }
    let ext = match kind {
        "photo" => "jpg",
        "voice" | "audio" => "ogg",
        "video" | "video_note" => "mp4",
        "sticker" => "webp",
        "gif" => "gif",
        _ => "bin",
    };
    format!("{message_id}_{kind}.{ext}")
}

fn created_text(label: &str, updates: &tl::enums::Updates) -> String {
    match created_chat(updates) {
        Some((id, title)) => format!("{label}: {title} ({id})."),
        None => format!("{label}."),
    }
}

fn created_chat(updates: &tl::enums::Updates) -> Option<(i64, String)> {
    let chats = match updates {
        tl::enums::Updates::Updates(updates) => &updates.chats,
        tl::enums::Updates::Combined(updates) => &updates.chats,
        _ => return None,
    };
    for chat in chats {
        match chat {
            tl::enums::Chat::Chat(chat) => {
                let id = PeerId::chat_unchecked(chat.id).bot_api_dialog_id_unchecked();
                return Some((id, chat.title.clone()));
            }
            tl::enums::Chat::Channel(chat) => {
                let id = PeerId::channel_unchecked(chat.id).bot_api_dialog_id_unchecked();
                return Some((id, chat.title.clone()));
            }
            _ => {}
        }
    }
    None
}

fn first_message_id(updates: &tl::enums::Updates) -> Option<i32> {
    let batch = match updates {
        tl::enums::Updates::Updates(updates) => &updates.updates,
        tl::enums::Updates::Combined(updates) => &updates.updates,
        _ => return None,
    };
    for update in batch {
        let message = match update {
            tl::enums::Update::NewMessage(update) => &update.message,
            tl::enums::Update::NewChannelMessage(update) => &update.message,
            _ => continue,
        };
        match message {
            tl::enums::Message::Message(message) => return Some(message.id),
            tl::enums::Message::Service(message) => return Some(message.id),
            tl::enums::Message::Empty(_) => {}
        }
    }
    None
}

fn topic_error(error: grammers_client::InvocationError) -> String {
    if let grammers_client::InvocationError::Rpc(error) = &error {
        if error.name.contains("FORUM") || error.name == "CHANNEL_FORUM_MISSING" {
            return "в этом чате нет тем".to_owned();
        }
    }
    invocation_message(error)
}

fn role_name(role: &Role) -> &'static str {
    match role {
        Role::Creator(_) => "creator",
        Role::Admin(_) => "admin",
        Role::Banned(_) => "banned",
        Role::Left(_) => "left",
        Role::User(_) => "member",
        _ => "member",
    }
}

fn format_date(seconds: i32) -> String {
    Timestamp::from_second(i64::from(seconds))
        .map(|time| time.to_string())
        .unwrap_or_else(|_| seconds.to_string())
}

fn random_id() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as i64)
        .unwrap_or(1)
}

fn peer_id(peer: &grammers_client::peer::Peer) -> Option<i64> {
    peer.id().bot_api_dialog_id()
}

fn peer_title(peer: &grammers_client::peer::Peer) -> String {
    match peer {
        grammers_client::peer::Peer::User(user) => user.full_name(),
        other => other.name().unwrap_or("").to_owned(),
    }
}
