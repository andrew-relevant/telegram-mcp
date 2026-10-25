//! Chat identity and the rules for turning a `chat_id` into one dialog.
//!
//! A numeric id or `@username` selects that chat. A title is used only when
//! exactly one dialog has that title. Several matches or a partial title
//! select nothing.

use grammers_client::sender::RpcError;
use grammers_client::InvocationError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatRecord {
    pub id: i64,
    pub title: String,
    pub username: Option<String>,
    pub kind: String,
    pub unread: i32,
    pub marked_unread: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageRecord {
    pub id: i32,
    pub chat_id: i64,
    pub chat_title: String,
    pub sender: String,
    pub text: String,
    pub date: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatTarget {
    Id(i64),
    Username(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedChat {
    One(i64),
    Username(String),
    Candidates(Vec<ChatRecord>),
    Missing,
}

impl ResolvedChat {
    pub fn target(self) -> Result<ChatTarget, Self> {
        match self {
            Self::One(id) => Ok(ChatTarget::Id(id)),
            Self::Username(name) => Ok(ChatTarget::Username(name)),
            other => Err(other),
        }
    }
}

pub fn resolve_chat(raw: &str, chats: &[ChatRecord]) -> ResolvedChat {
    let query = raw.trim();
    if query.is_empty() {
        return ResolvedChat::Missing;
    }
    if let Some(username) = query.strip_prefix('@') {
        let username = username.trim();
        if username.is_empty() {
            return ResolvedChat::Missing;
        }
        let matches: Vec<_> = chats
            .iter()
            .filter(|chat| {
                chat.username
                    .as_deref()
                    .is_some_and(|name| name.eq_ignore_ascii_case(username))
            })
            .cloned()
            .collect();
        return match matches.len() {
            0 => ResolvedChat::Username(username.to_owned()),
            1 => ResolvedChat::One(matches[0].id),
            _ => ResolvedChat::Candidates(matches),
        };
    }
    if let Some(id) = parse_chat_id(query) {
        return ResolvedChat::One(id);
    }
    let exact: Vec<_> = chats
        .iter()
        .filter(|chat| chat.title == query)
        .cloned()
        .collect();
    if exact.len() == 1 {
        return ResolvedChat::One(exact[0].id);
    }
    if exact.len() > 1 {
        return ResolvedChat::Candidates(exact);
    }
    let partial: Vec<_> = chats
        .iter()
        .filter(|chat| {
            !chat.title.is_empty() && chat.title.to_lowercase().contains(&query.to_lowercase())
        })
        .cloned()
        .collect();
    if partial.is_empty() {
        ResolvedChat::Missing
    } else {
        ResolvedChat::Candidates(partial)
    }
}

fn parse_chat_id(query: &str) -> Option<i64> {
    let digits = query.strip_prefix('-').unwrap_or(query);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    query.parse().ok()
}

/// grammers already waits out a flood of at most 60 seconds.
/// A longer wait is returned to the caller instead of blocking.
pub fn flood_wait_message(error: &InvocationError) -> Option<String> {
    let InvocationError::Rpc(RpcError {
        name,
        value: Some(seconds),
        ..
    }) = error
    else {
        return None;
    };
    if name == "FLOOD_WAIT" && *seconds > 60 {
        Some(format!("Telegram просит подождать {seconds} с"))
    } else {
        None
    }
}

pub fn invocation_message(error: InvocationError) -> String {
    flood_wait_message(&error).unwrap_or_else(|| "запрос к Telegram не выполнен".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(id: i64, title: &str, username: Option<&str>) -> ChatRecord {
        ChatRecord {
            id,
            title: title.to_owned(),
            username: username.map(str::to_owned),
            kind: "user".to_owned(),
            unread: 0,
            marked_unread: false,
        }
    }

    #[test]
    fn exact_title_selects_one_chat() {
        let chats = vec![chat(1, "Ada", None), chat(2, "Grace", None)];
        assert_eq!(resolve_chat("Ada", &chats), ResolvedChat::One(1));
    }

    #[test]
    fn repeated_title_returns_candidates() {
        let chats = vec![chat(1, "Ada", None), chat(2, "Ada", None)];
        match resolve_chat("Ada", &chats) {
            ResolvedChat::Candidates(found) => {
                assert_eq!(found.len(), 2);
            }
            other => panic!("expected candidates, got {other:?}"),
        }
    }

    #[test]
    fn partial_title_returns_candidates() {
        let chats = vec![chat(1, "Ada Lovelace", None), chat(2, "Grace", None)];
        match resolve_chat("Ada", &chats) {
            ResolvedChat::Candidates(found) => assert_eq!(found[0].id, 1),
            other => panic!("expected candidates, got {other:?}"),
        }
    }

    #[test]
    fn numeric_id_and_username_are_direct() {
        let chats = vec![chat(7, "Ada", Some("ada"))];
        assert_eq!(resolve_chat("7", &chats), ResolvedChat::One(7));
        assert_eq!(resolve_chat("-1001", &chats), ResolvedChat::One(-1001));
        assert_eq!(resolve_chat("@ada", &chats), ResolvedChat::One(7));
        assert_eq!(
            resolve_chat("@missing", &chats),
            ResolvedChat::Username("missing".to_owned())
        );
    }

    #[test]
    fn long_flood_wait_is_reported() {
        let short = InvocationError::Rpc(RpcError {
            code: 420,
            name: "FLOOD_WAIT".to_owned(),
            value: Some(60),
            caused_by: None,
        });
        let long = InvocationError::Rpc(RpcError {
            code: 420,
            name: "FLOOD_WAIT".to_owned(),
            value: Some(61),
            caused_by: None,
        });
        assert!(flood_wait_message(&short).is_none());
        assert_eq!(
            flood_wait_message(&long).as_deref(),
            Some("Telegram просит подождать 61 с")
        );
    }
}
