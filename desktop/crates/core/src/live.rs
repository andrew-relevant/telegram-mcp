//! grammers-backed chat operations.
//!
//! The caller owns the logged-in [`Client`]. Flood waits of at most 60 seconds
//! are slept by grammers' default retry policy. Longer waits become text.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use grammers_client::client::{GlobalSearchIter, MessageIter, SearchIter};
use grammers_client::message::{InputMessage, InputReactions};
use grammers_client::peer::{Dialog, Peer};
use grammers_client::session::types::PeerId;
use grammers_client::session::updates::UpdatesLike;
use grammers_client::tl;
use grammers_client::update::Update;
use grammers_client::Client;
use tokio::sync::mpsc;

use crate::chats::{
    invocation_message, ChatRecord, ChatTarget, MessageRecord, ParticipantRecord, TopicRecord,
};
use crate::files;
use crate::ops;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait Telegram: Send + Sync {
    fn dialogs<'a>(&'a self, limit: usize) -> BoxFuture<'a, Result<Vec<ChatRecord>, String>>;
    fn chat<'a>(&'a self, target: &'a ChatTarget) -> BoxFuture<'a, Result<ChatRecord, String>>;
    fn history<'a>(
        &'a self,
        target: &'a ChatTarget,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<MessageRecord>, String>>;
    fn search_messages<'a>(
        &'a self,
        target: &'a ChatTarget,
        query: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<MessageRecord>, String>>;
    fn search_global<'a>(
        &'a self,
        query: &'a str,
        skip: usize,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<MessageRecord>, String>>;
    fn send_message<'a>(
        &'a self,
        target: &'a ChatTarget,
        message: &'a str,
    ) -> BoxFuture<'a, Result<(), String>>;
    fn reply_to_message<'a>(
        &'a self,
        target: &'a ChatTarget,
        message_id: i32,
        text: &'a str,
    ) -> BoxFuture<'a, Result<(), String>>;
    fn history_topic<'a>(
        &'a self,
        target: &'a ChatTarget,
        topic_id: i32,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<MessageRecord>, String>> {
        let _ = (target, topic_id, limit);
        unsupported()
    }
    fn search_topic<'a>(
        &'a self,
        target: &'a ChatTarget,
        query: &'a str,
        topic_id: i32,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<MessageRecord>, String>> {
        let _ = (target, query, topic_id, limit);
        unsupported()
    }
    fn download_media<'a>(
        &'a self,
        target: &'a ChatTarget,
        message_id: i32,
        file_path: Option<String>,
    ) -> BoxFuture<'a, Result<String, String>> {
        let _ = (target, message_id, file_path);
        unsupported()
    }
    fn send_file<'a>(
        &'a self,
        target: &'a ChatTarget,
        path: &'a str,
        caption: &'a str,
        topic_id: Option<i32>,
        kind: &'a str,
        emoji: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), String>> {
        let _ = (target, path, caption, topic_id, kind, emoji);
        unsupported()
    }
    fn edit_message<'a>(
        &'a self,
        target: &'a ChatTarget,
        message_id: i32,
        text: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        let _ = (target, message_id, text);
        unsupported()
    }
    fn delete_message<'a>(
        &'a self,
        target: &'a ChatTarget,
        message_id: i32,
    ) -> BoxFuture<'a, Result<(), String>> {
        let _ = (target, message_id);
        unsupported()
    }
    fn forward_message<'a>(
        &'a self,
        source: &'a ChatTarget,
        destination: &'a ChatTarget,
        message_id: i32,
        topic_id: Option<i32>,
    ) -> BoxFuture<'a, Result<(), String>> {
        let _ = (source, destination, message_id, topic_id);
        unsupported()
    }
    fn react<'a>(
        &'a self,
        target: &'a ChatTarget,
        message_id: i32,
        emoji: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), String>> {
        let _ = (target, message_id, emoji);
        unsupported()
    }
    fn pin_message<'a>(
        &'a self,
        target: &'a ChatTarget,
        message_id: i32,
        pin: bool,
    ) -> BoxFuture<'a, Result<(), String>> {
        let _ = (target, message_id, pin);
        unsupported()
    }
    fn pinned_messages<'a>(
        &'a self,
        target: &'a ChatTarget,
    ) -> BoxFuture<'a, Result<Vec<MessageRecord>, String>> {
        let _ = target;
        unsupported()
    }
    fn participants<'a>(
        &'a self,
        target: &'a ChatTarget,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<ParticipantRecord>, String>> {
        let _ = (target, limit);
        unsupported()
    }
    fn invite_user<'a>(
        &'a self,
        chat: &'a ChatTarget,
        user: &'a ChatTarget,
    ) -> BoxFuture<'a, Result<(), String>> {
        let _ = (chat, user);
        unsupported()
    }
    fn remove_user<'a>(
        &'a self,
        chat: &'a ChatTarget,
        user: &'a ChatTarget,
    ) -> BoxFuture<'a, Result<(), String>> {
        let _ = (chat, user);
        unsupported()
    }
    fn create_group<'a>(
        &'a self,
        title: &'a str,
        users: &'a [ChatTarget],
    ) -> BoxFuture<'a, Result<String, String>> {
        let _ = (title, users);
        unsupported()
    }
    fn create_channel<'a>(
        &'a self,
        title: &'a str,
        about: &'a str,
        megagroup: bool,
        forum: bool,
    ) -> BoxFuture<'a, Result<String, String>> {
        let _ = (title, about, megagroup, forum);
        unsupported()
    }
    fn list_topics<'a>(
        &'a self,
        target: &'a ChatTarget,
        query: Option<&'a str>,
        offset_topic: i32,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<TopicRecord>, String>> {
        let _ = (target, query, offset_topic, limit);
        unsupported()
    }
    fn create_topic<'a>(
        &'a self,
        target: &'a ChatTarget,
        title: &'a str,
    ) -> BoxFuture<'a, Result<String, String>> {
        let _ = (target, title);
        unsupported()
    }
}

fn unsupported<'a, T: Send + 'a>() -> BoxFuture<'a, Result<T, String>> {
    Box::pin(async { Err("не поддерживается".to_owned()) })
}

#[derive(Clone)]
pub struct LiveTelegram {
    client: Client,
}

impl LiveTelegram {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

impl Telegram for LiveTelegram {
    fn dialogs<'a>(&'a self, limit: usize) -> BoxFuture<'a, Result<Vec<ChatRecord>, String>> {
        let client = self.client.clone();
        Box::pin(async move {
            let mut iter = client.iter_dialogs().limit(limit);
            let mut chats = Vec::new();
            while let Some(dialog) = iter.next().await.map_err(invocation_message)? {
                if let Some(record) = chat_from_dialog(&dialog) {
                    chats.push(record);
                }
            }
            Ok(chats)
        })
    }

    fn chat<'a>(&'a self, target: &'a ChatTarget) -> BoxFuture<'a, Result<ChatRecord, String>> {
        let client = self.client.clone();
        Box::pin(async move { record_for(&client, target).await })
    }

    fn history<'a>(
        &'a self,
        target: &'a ChatTarget,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<MessageRecord>, String>> {
        let client = self.client.clone();
        Box::pin(async move {
            let peer = peer_ref_for(&client, target).await?;
            let mut iter = client.iter_messages(peer).limit(limit);
            collect_messages(&mut iter).await
        })
    }

    fn search_messages<'a>(
        &'a self,
        target: &'a ChatTarget,
        query: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<MessageRecord>, String>> {
        let client = self.client.clone();
        let query = query.to_owned();
        Box::pin(async move {
            let peer = peer_ref_for(&client, target).await?;
            let mut iter = client.search_messages(peer).query(&query).limit(limit);
            collect_search(&mut iter).await
        })
    }

    fn search_global<'a>(
        &'a self,
        query: &'a str,
        skip: usize,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<MessageRecord>, String>> {
        let client = self.client.clone();
        let query = query.to_owned();
        Box::pin(async move {
            let mut iter = client
                .search_all_messages()
                .query(&query)
                .limit(skip.saturating_add(limit));
            collect_global(&mut iter, skip, limit).await
        })
    }

    fn send_message<'a>(
        &'a self,
        target: &'a ChatTarget,
        message: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        let client = self.client.clone();
        let message = message.to_owned();
        Box::pin(async move {
            let peer = peer_ref_for(&client, target).await?;
            client
                .send_message(peer, message)
                .await
                .map_err(invocation_message)?;
            Ok(())
        })
    }

    fn reply_to_message<'a>(
        &'a self,
        target: &'a ChatTarget,
        message_id: i32,
        text: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        let client = self.client.clone();
        let text = text.to_owned();
        Box::pin(async move {
            let peer = peer_ref_for(&client, target).await?;
            client
                .send_message(
                    peer,
                    InputMessage::new().text(text).reply_to(Some(message_id)),
                )
                .await
                .map_err(invocation_message)?;
            Ok(())
        })
    }

    fn history_topic<'a>(
        &'a self,
        target: &'a ChatTarget,
        topic_id: i32,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<MessageRecord>, String>> {
        let client = self.client.clone();
        Box::pin(async move {
            let chat = record_for(&client, target).await?;
            let peer = peer_ref_for(&client, target).await?;
            ops::history_topic(&client, peer, chat.id, &chat.title, topic_id, limit).await
        })
    }

    fn search_topic<'a>(
        &'a self,
        target: &'a ChatTarget,
        query: &'a str,
        topic_id: i32,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<MessageRecord>, String>> {
        let client = self.client.clone();
        let query = query.to_owned();
        Box::pin(async move {
            let chat = record_for(&client, target).await?;
            let peer = peer_ref_for(&client, target).await?;
            ops::search_topic(&client, peer, chat.id, &chat.title, &query, topic_id, limit).await
        })
    }

    fn download_media<'a>(
        &'a self,
        target: &'a ChatTarget,
        message_id: i32,
        file_path: Option<String>,
    ) -> BoxFuture<'a, Result<String, String>> {
        let client = self.client.clone();
        Box::pin(async move {
            let peer = peer_ref_for(&client, target).await?;
            ops::download_media(&client, peer, message_id, file_path.as_deref()).await
        })
    }

    fn send_file<'a>(
        &'a self,
        target: &'a ChatTarget,
        path: &'a str,
        caption: &'a str,
        topic_id: Option<i32>,
        kind: &'a str,
        emoji: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), String>> {
        let client = self.client.clone();
        let path = path.to_owned();
        let caption = caption.to_owned();
        let kind = kind.to_owned();
        let emoji = emoji.map(str::to_owned);
        Box::pin(async move {
            let file = files::readable_file(&path)?;
            let peer = peer_ref_for(&client, target).await?;
            ops::send_path(
                &client,
                peer,
                &file,
                &caption,
                topic_id,
                &kind,
                emoji.as_deref(),
            )
            .await
        })
    }

    fn edit_message<'a>(
        &'a self,
        target: &'a ChatTarget,
        message_id: i32,
        text: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        let client = self.client.clone();
        let text = text.to_owned();
        Box::pin(async move {
            let peer = peer_ref_for(&client, target).await?;
            client
                .edit_message(peer, message_id, text)
                .await
                .map_err(invocation_message)
        })
    }

    fn delete_message<'a>(
        &'a self,
        target: &'a ChatTarget,
        message_id: i32,
    ) -> BoxFuture<'a, Result<(), String>> {
        let client = self.client.clone();
        Box::pin(async move {
            let peer = peer_ref_for(&client, target).await?;
            client
                .delete_messages(peer, &[message_id])
                .await
                .map_err(invocation_message)?;
            Ok(())
        })
    }

    fn forward_message<'a>(
        &'a self,
        source: &'a ChatTarget,
        destination: &'a ChatTarget,
        message_id: i32,
        topic_id: Option<i32>,
    ) -> BoxFuture<'a, Result<(), String>> {
        let client = self.client.clone();
        Box::pin(async move {
            let from = peer_ref_for(&client, source).await?;
            let to = peer_ref_for(&client, destination).await?;
            if let Some(topic_id) = topic_id {
                return ops::forward_to_topic(&client, from, to, message_id, topic_id).await;
            }
            client
                .forward_messages(to, &[message_id], from)
                .await
                .map_err(invocation_message)?;
            Ok(())
        })
    }

    fn react<'a>(
        &'a self,
        target: &'a ChatTarget,
        message_id: i32,
        emoji: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), String>> {
        let client = self.client.clone();
        let emoji = emoji.map(str::to_owned);
        Box::pin(async move {
            let peer = peer_ref_for(&client, target).await?;
            let reactions = match emoji.as_deref() {
                Some(emoji) => InputReactions::emoticon(emoji),
                None => InputReactions::remove(),
            };
            client
                .send_reactions(peer, message_id, reactions)
                .await
                .map_err(invocation_message)
        })
    }

    fn pin_message<'a>(
        &'a self,
        target: &'a ChatTarget,
        message_id: i32,
        pin: bool,
    ) -> BoxFuture<'a, Result<(), String>> {
        let client = self.client.clone();
        Box::pin(async move {
            let peer = peer_ref_for(&client, target).await?;
            let result = if pin {
                client.pin_message(peer, message_id).await
            } else {
                client.unpin_message(peer, message_id).await
            };
            result.map_err(invocation_message)
        })
    }

    fn pinned_messages<'a>(
        &'a self,
        target: &'a ChatTarget,
    ) -> BoxFuture<'a, Result<Vec<MessageRecord>, String>> {
        let client = self.client.clone();
        Box::pin(async move {
            let peer = peer_ref_for(&client, target).await?;
            ops::pinned(&client, peer).await
        })
    }

    fn participants<'a>(
        &'a self,
        target: &'a ChatTarget,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<ParticipantRecord>, String>> {
        let client = self.client.clone();
        Box::pin(async move {
            let peer = peer_ref_for(&client, target).await?;
            ops::participants(&client, peer, limit).await
        })
    }

    fn invite_user<'a>(
        &'a self,
        chat: &'a ChatTarget,
        user: &'a ChatTarget,
    ) -> BoxFuture<'a, Result<(), String>> {
        let client = self.client.clone();
        Box::pin(async move {
            let chat = peer_ref_for(&client, chat).await?;
            let user = peer_ref_for(&client, user).await?;
            ops::invite(&client, chat, user).await
        })
    }

    fn remove_user<'a>(
        &'a self,
        chat: &'a ChatTarget,
        user: &'a ChatTarget,
    ) -> BoxFuture<'a, Result<(), String>> {
        let client = self.client.clone();
        Box::pin(async move {
            let chat = peer_ref_for(&client, chat).await?;
            let user = peer_ref_for(&client, user).await?;
            client
                .kick_participant(chat, user)
                .await
                .map_err(invocation_message)
        })
    }

    fn create_group<'a>(
        &'a self,
        title: &'a str,
        users: &'a [ChatTarget],
    ) -> BoxFuture<'a, Result<String, String>> {
        let client = self.client.clone();
        let title = title.to_owned();
        let users = users.to_vec();
        Box::pin(async move {
            let mut peers = Vec::with_capacity(users.len());
            for user in &users {
                peers.push(peer_ref_for(&client, user).await?);
            }
            ops::create_group(&client, &title, &peers).await
        })
    }

    fn create_channel<'a>(
        &'a self,
        title: &'a str,
        about: &'a str,
        megagroup: bool,
        forum: bool,
    ) -> BoxFuture<'a, Result<String, String>> {
        let client = self.client.clone();
        let title = title.to_owned();
        let about = about.to_owned();
        Box::pin(async move { ops::create_channel(&client, &title, &about, megagroup, forum).await })
    }

    fn list_topics<'a>(
        &'a self,
        target: &'a ChatTarget,
        query: Option<&'a str>,
        offset_topic: i32,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<TopicRecord>, String>> {
        let client = self.client.clone();
        let query = query.map(str::to_owned);
        Box::pin(async move {
            let peer = peer_ref_for(&client, target).await?;
            ops::list_topics(&client, peer, query.as_deref(), offset_topic, limit).await
        })
    }

    fn create_topic<'a>(
        &'a self,
        target: &'a ChatTarget,
        title: &'a str,
    ) -> BoxFuture<'a, Result<String, String>> {
        let client = self.client.clone();
        let title = title.to_owned();
        Box::pin(async move {
            let peer = peer_ref_for(&client, target).await?;
            ops::create_topic(&client, peer, &title).await
        })
    }
}

async fn peer_ref_for(
    client: &Client,
    target: &ChatTarget,
) -> Result<grammers_client::session::types::PeerRef, String> {
    match target {
        ChatTarget::Id(id) => Ok(find_dialog(client, *id).await?.peer_ref()),
        ChatTarget::Username(username) => username_ref(client, username).await,
    }
}

async fn record_for(client: &Client, target: &ChatTarget) -> Result<ChatRecord, String> {
    match target {
        ChatTarget::Id(id) => {
            let dialog = find_dialog(client, *id).await?;
            chat_from_dialog(&dialog).ok_or_else(|| "чат не найден".to_owned())
        }
        ChatTarget::Username(username) => {
            let peer = client
                .resolve_username(username)
                .await
                .map_err(invocation_message)?
                .ok_or_else(|| "чат не найден".to_owned())?;
            chat_from_peer(&peer).ok_or_else(|| "чат не найден".to_owned())
        }
    }
}

async fn username_ref(
    client: &Client,
    username: &str,
) -> Result<grammers_client::session::types::PeerRef, String> {
    let peer = client
        .resolve_username(username)
        .await
        .map_err(invocation_message)?
        .ok_or_else(|| "чат не найден".to_owned())?;
    peer.to_ref()
        .await
        .ok()
        .flatten()
        .ok_or_else(|| "чат не найден".to_owned())
}

async fn find_dialog(client: &Client, id: i64) -> Result<Dialog, String> {
    let Some(peer_id) = PeerId::from_bot_api_dialog_id(id) else {
        return Err("чат не найден".to_owned());
    };
    let mut iter = client.iter_dialogs();
    while let Some(dialog) = iter.next().await.map_err(invocation_message)? {
        if dialog.peer().id() == peer_id {
            return Ok(dialog);
        }
    }
    Err("чат не найден".to_owned())
}

async fn collect_messages(iter: &mut MessageIter) -> Result<Vec<MessageRecord>, String> {
    let mut messages = Vec::new();
    while let Some(message) = iter.next().await.map_err(invocation_message)? {
        messages.push(ops::message_record(&message));
    }
    Ok(messages)
}

async fn collect_search(iter: &mut SearchIter) -> Result<Vec<MessageRecord>, String> {
    let mut messages = Vec::new();
    while let Some(message) = iter.next().await.map_err(invocation_message)? {
        messages.push(ops::message_record(&message));
    }
    Ok(messages)
}

async fn collect_global(
    iter: &mut GlobalSearchIter,
    skip: usize,
    limit: usize,
) -> Result<Vec<MessageRecord>, String> {
    let mut messages = Vec::new();
    let mut seen = 0usize;
    while messages.len() < limit {
        let Some(message) = iter.next().await.map_err(invocation_message)? else {
            break;
        };
        if seen < skip {
            seen += 1;
            continue;
        }
        messages.push(ops::message_record(&message));
    }
    Ok(messages)
}

fn chat_from_dialog(dialog: &Dialog) -> Option<ChatRecord> {
    let peer = dialog.peer();
    let (unread, marked_unread) = match &dialog.raw {
        tl::enums::Dialog::Dialog(inner) => (inner.unread_count, inner.unread_mark),
        _ => (0, false),
    };
    Some(ChatRecord {
        id: peer_id(peer)?,
        title: peer_title(peer),
        username: peer.username().map(str::to_owned),
        kind: peer_kind(peer).to_owned(),
        unread,
        marked_unread,
    })
}

fn chat_from_peer(peer: &Peer) -> Option<ChatRecord> {
    Some(ChatRecord {
        id: peer_id(peer)?,
        title: peer_title(peer),
        username: peer.username().map(str::to_owned),
        kind: peer_kind(peer).to_owned(),
        unread: 0,
        marked_unread: false,
    })
}

fn peer_id(peer: &Peer) -> Option<i64> {
    peer.id().bot_api_dialog_id()
}

fn peer_title(peer: &Peer) -> String {
    match peer {
        Peer::User(user) => user.full_name(),
        other => other.name().unwrap_or("").to_owned(),
    }
}

fn peer_kind(peer: &Peer) -> &'static str {
    match peer {
        Peer::User(_) => "user",
        Peer::Group(_) => "group",
        Peer::Channel(_) => "channel",
        Peer::Community(_) => "community",
    }
}

/// Forward new messages into `hook` until the update stream ends.
pub fn spawn_incoming_watch(
    client: Client,
    updates: mpsc::Receiver<UpdatesLike>,
    hook: Arc<dyn Fn(MessageRecord) + Send + Sync>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut stream = match client
            .stream_updates(
                updates,
                grammers_client::sender::UpdatesConfiguration::default(),
            )
            .await
        {
            Ok(stream) => stream,
            Err(_) => return,
        };
        loop {
            match stream.next().await {
                Ok(Update::NewMessage(message)) => hook(ops::message_record(&message)),
                Ok(_) => {}
                Err(_) => break,
            }
        }
    })
}
