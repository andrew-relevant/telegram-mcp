//! grammers-backed chat operations.
//!
//! The caller owns the logged-in [`Client`]. Flood waits of at most 60 seconds
//! are slept by grammers' default retry policy. Longer waits become text.

use std::future::Future;
use std::pin::Pin;

use grammers_client::client::{GlobalSearchIter, MessageIter, SearchIter};
use grammers_client::message::{InputMessage, Message};
use grammers_client::peer::{Dialog, Peer};
use grammers_client::session::types::PeerId;
use grammers_client::tl;
use grammers_client::Client;

use crate::chats::{invocation_message, ChatRecord, ChatTarget, MessageRecord};

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
        messages.push(message_record(&message));
    }
    Ok(messages)
}

async fn collect_search(iter: &mut SearchIter) -> Result<Vec<MessageRecord>, String> {
    let mut messages = Vec::new();
    while let Some(message) = iter.next().await.map_err(invocation_message)? {
        messages.push(message_record(&message));
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
        messages.push(message_record(&message));
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

fn message_record(message: &Message) -> MessageRecord {
    let peer = message.peer();
    MessageRecord {
        id: message.id(),
        chat_id: peer.and_then(peer_id).unwrap_or(0),
        chat_title: peer.map(peer_title).unwrap_or_default(),
        sender: message
            .sender()
            .map(peer_title)
            .filter(|name| !name.is_empty())
            .unwrap_or_default(),
        text: message.text().to_owned(),
        date: message.date().to_string(),
    }
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
