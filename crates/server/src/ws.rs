//! One WebSocket per attached client. The socket replays the session log
//! from `?after=<seq>` and then follows it live; closing it never stops the
//! session.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use unchained_core::Session;
use unchained_protocol::{ClientMessage, Event};

use crate::AppState;

const MAX_MESSAGE_BYTES: usize = 256 * 1024;

#[derive(Deserialize)]
pub struct Params {
    after: Option<u64>,
}

pub async fn handler(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<Params>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some(session) = st.manager.get(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    upgrade
        .max_message_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            st.clients.fetch_add(1, Ordering::Relaxed);
            client_loop(socket, session, params.after.unwrap_or(0)).await;
            st.clients.fetch_sub(1, Ordering::Relaxed);
        })
}

async fn send_event(socket: &mut WebSocket, event: &Event) -> bool {
    match serde_json::to_string(event) {
        Ok(text) => socket.send(Message::Text(text.into())).await.is_ok(),
        Err(_) => false,
    }
}

async fn client_loop(mut socket: WebSocket, session: Arc<Session>, after: u64) {
    let (replay, mut live) = session.subscribe_from(after);
    for event in &replay {
        if !send_event(&mut socket, event).await {
            return;
        }
    }
    loop {
        tokio::select! {
            event = live.recv() => match event {
                Ok(event) => {
                    if !send_event(&mut socket, &event).await {
                        break;
                    }
                }
                // Fell too far behind: drop the socket; the client reconnects
                // with its last seq and replays from the log.
                Err(RecvError::Lagged(_)) | Err(RecvError::Closed) => break,
            },
            message = socket.recv() => match message {
                Some(Ok(Message::Text(text))) => {
                    if let Some(reason) = dispatch(&session, text.as_str()) {
                        let notice = json!({ "type": "rejected", "reason": reason }).to_string();
                        if socket.send(Message::Text(notice.into())).await.is_err() {
                            break;
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
        }
    }
}

/// Apply a client message. Returns a reason if it was rejected.
fn dispatch(session: &Arc<Session>, text: &str) -> Option<String> {
    match serde_json::from_str::<ClientMessage>(text) {
        Ok(ClientMessage::UserMessage { text }) => {
            if text.trim().is_empty() {
                return Some("empty message".into());
            }
            session
                .submit_user_message(text)
                .err()
                .map(|e| e.to_string())
        }
        Ok(ClientMessage::PermissionDecision { call_id, allow }) => {
            if session.resolve_permission(&call_id, allow) {
                None
            } else {
                Some("no pending request with that call_id".into())
            }
        }
        Ok(ClientMessage::Interrupt) => {
            session.interrupt();
            None
        }
        Err(e) => Some(format!("invalid message: {e}")),
    }
}
