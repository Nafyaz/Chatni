use crate::config::Config;
use crate::AppState;
use anyhow::{Context, Result};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use tracing::{error, info, warn};

/// Normalized representation of an incoming Google Chat message event
#[derive(Debug, Clone)]
pub struct NormalizedChatEvent {
    pub event_type: String,
    pub is_workspace_addon: bool,
    pub verification_token: Option<String>,
    pub sender_name: String,
    pub space_name: String,
    pub thread_name: String,
    pub cleaned_text: String,
}

impl NormalizedChatEvent {
    pub fn from_payload(payload: &Value, bot_name: &str) -> Option<Self> {
        // 1. Check Google Workspace Add-ons Chat event format (`payload.chat.messagePayload`)
        if let Some(chat_obj) = payload.get("chat") {
            let sender_name = chat_obj
                .pointer("/user/displayName")
                .and_then(|v| v.as_str())
                .unwrap_or("Teammate")
                .to_string();

            if let Some(msg_payload) = chat_obj.get("messagePayload") {
                let space_name = msg_payload
                    .pointer("/space/name")
                    .or_else(|| msg_payload.pointer("/message/space/name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("spaces/default")
                    .to_string();

                let thread_name = msg_payload
                    .pointer("/message/thread/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&space_name)
                    .to_string();

                let raw_text = msg_payload
                    .pointer("/message/argumentText")
                    .or_else(|| msg_payload.pointer("/message/text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let cleaned_text = clean_mention(raw_text, bot_name);

                return Some(Self {
                    event_type: "MESSAGE".to_string(),
                    is_workspace_addon: true,
                    verification_token: payload
                        .pointer("/commonEventObject/invokedFunction")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    sender_name,
                    space_name,
                    thread_name,
                    cleaned_text,
                });
            }

            if chat_obj.get("addedToSpacePayload").is_some() {
                return Some(Self {
                    event_type: "ADDED_TO_SPACE".to_string(),
                    is_workspace_addon: true,
                    verification_token: None,
                    sender_name,
                    space_name: "spaces/default".to_string(),
                    thread_name: "spaces/default".to_string(),
                    cleaned_text: String::new(),
                });
            }
        }

        // 2. Classic Google Chat HTTP Event format (`type: "MESSAGE"`, `message: {...}`)
        let event_type = payload
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("MESSAGE")
            .to_string();

        let verification_token = payload
            .get("token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        if event_type == "ADDED_TO_SPACE" {
            let sender_name = payload
                .pointer("/user/displayName")
                .and_then(|v| v.as_str())
                .unwrap_or("Teammate")
                .to_string();
            let space_name = payload
                .pointer("/space/name")
                .and_then(|v| v.as_str())
                .unwrap_or("spaces/default")
                .to_string();

            return Some(Self {
                event_type,
                is_workspace_addon: false,
                verification_token,
                sender_name,
                space_name: space_name.clone(),
                thread_name: space_name,
                cleaned_text: String::new(),
            });
        }

        // Extract text from either `message.argumentText`, `message.text`, or top-level `text` (for easy curl testing)
        let raw_text = payload
            .pointer("/message/argumentText")
            .or_else(|| payload.pointer("/message/text"))
            .or_else(|| payload.get("text"))
            .and_then(|v| v.as_str())?;

        let sender_name = payload
            .pointer("/message/sender/displayName")
            .or_else(|| payload.pointer("/user/displayName"))
            .or_else(|| payload.get("sender"))
            .and_then(|v| v.as_str())
            .unwrap_or("Teammate")
            .to_string();

        let space_name = payload
            .pointer("/space/name")
            .or_else(|| payload.pointer("/message/space/name"))
            .and_then(|v| v.as_str())
            .unwrap_or("spaces/default")
            .to_string();

        let thread_name = payload
            .pointer("/message/thread/name")
            .or_else(|| payload.get("thread_id"))
            .and_then(|v| v.as_str())
            .unwrap_or(&space_name)
            .to_string();

        let cleaned_text = clean_mention(raw_text, bot_name);

        Some(Self {
            event_type,
            is_workspace_addon: false,
            verification_token,
            sender_name,
            space_name,
            thread_name,
            cleaned_text,
        })
    }

    pub fn format_sync_reply(&self, text: &str, thread_name: &str) -> Value {
        if self.is_workspace_addon {
            json!({
                "hostAppDataAction": {
                    "chatDataAction": {
                        "createMessageAction": {
                            "message": {
                                "text": text,
                                "thread": {
                                    "name": thread_name
                                }
                            }
                        }
                    }
                }
            })
        } else {
            json!({
                "text": text,
                "thread": {
                    "name": thread_name
                }
            })
        }
    }
}

/// Strips leading `@bot_name` mention (e.g. `@gemini, please review ...`) while preserving message content
pub fn clean_mention(raw: &str, bot_name: &str) -> String {
    let trimmed = raw.trim();
    let mention_prefix = format!("@{}", bot_name.to_lowercase());

    if trimmed.to_lowercase().starts_with(&mention_prefix) {
        let after = &trimmed[mention_prefix.len()..];
        return after
            .trim_start_matches(|c: char| c == ',' || c == ':' || c.is_whitespace())
            .to_string();
    }
    trimmed.to_string()
}

/// Service account credentials JSON structure for Google Chat REST API async replies
#[derive(Debug, Deserialize)]
struct ServiceAccountKey {
    client_email: String,
    private_key: String,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}

fn default_token_uri() -> String {
    "https://oauth2.googleapis.com/token".to_string()
}

#[derive(Debug, Serialize)]
struct JwtClaims {
    iss: String,
    scope: String,
    aud: String,
    exp: i64,
    iat: i64,
}

#[derive(Debug, Deserialize)]
struct AccessTokenResponse {
    access_token: String,
}

pub async fn post_async_google_chat_message(
    config: &Config,
    space_name: &str,
    thread_name: &str,
    text: &str,
) -> Result<()> {
    let http = Client::new();

    // Option 1: Incoming Space Webhook URL (`GOOGLE_CHAT_WEBHOOK_URL`)
    if let Some(webhook_url) = &config.google_chat_webhook_url {
        let separator = if webhook_url.contains('?') { "&" } else { "?" };
        let url = format!(
            "{}{}messageReplyOption=REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD",
            webhook_url, separator
        );
        let body = json!({
            "text": text,
            "thread": {
                "name": thread_name
            }
        });
        let resp = http.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            let err_body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Failed to post via Google Chat webhook URL: {}", err_body);
        }
        return Ok(());
    }

    // Option 2: Google Service Account (`GOOGLE_SERVICE_ACCOUNT_JSON`) via Google Chat REST API
    if let Some(sa_cfg) = &config.google_service_account_json {
        let raw_json = if Path::new(sa_cfg).exists() {
            std::fs::read_to_string(sa_cfg)
                .with_context(|| format!("Failed to read service account file {}", sa_cfg))?
        } else {
            sa_cfg.clone()
        };

        let sa: ServiceAccountKey = serde_json::from_str(&raw_json)
            .context("Failed to parse GOOGLE_SERVICE_ACCOUNT_JSON")?;

        let now = Utc::now().timestamp();
        let claims = JwtClaims {
            iss: sa.client_email,
            scope: "https://www.googleapis.com/auth/chat.bot".to_string(),
            aud: sa.token_uri.clone(),
            iat: now,
            exp: now + 3600,
        };

        let key = EncodingKey::from_rsa_pem(sa.private_key.as_bytes())
            .context("Invalid RSA private key in GOOGLE_SERVICE_ACCOUNT_JSON")?;
        let jwt = encode(&Header::new(Algorithm::RS256), &claims, &key)?;

        let token_resp: AccessTokenResponse = http
            .post(&sa.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", &jwt),
            ])
            .send()
            .await?
            .json()
            .await
            .context("Failed to exchange Google Service Account JWT for access token")?;

        let url = format!(
            "https://chat.googleapis.com/v1/{}/messages?messageReplyOption=REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD",
            space_name.trim_start_matches('/')
        );

        let body = json!({
            "text": text,
            "thread": {
                "name": thread_name
            }
        });

        let resp = http
            .post(&url)
            .bearer_auth(&token_resp.access_token)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err_body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Google Chat REST API error: {}", err_body);
        }
        return Ok(());
    }

    anyhow::bail!("Neither GOOGLE_CHAT_WEBHOOK_URL nor GOOGLE_SERVICE_ACCOUNT_JSON is configured for async replies")
}

pub async fn google_chat_webhook_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> Response {
    let Some(event) = NormalizedChatEvent::from_payload(&payload, &state.config.bot_name) else {
        return (StatusCode::OK, Json(json!({}))).into_response();
    };

    // Verify token if configured
    if let Some(expected_token) = &state.config.google_chat_verification_token {
        if let Some(actual_token) = &event.verification_token {
            if actual_token != expected_token {
                warn!("Rejected Google Chat request with invalid verification token");
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "error": "Invalid verification token" })),
                )
                    .into_response();
            }
        }
    }

    if event.event_type == "ADDED_TO_SPACE" {
        let greeting = format!(
            "Hey team! 👋 I'm *@{bot}*, your AI engineering teammate.\nMention me in any thread like `@{bot}, please review the pr https://github.com/Nafyaz/Homelab/pull/1` and I'll jump right in!",
            bot = state.config.bot_name
        );
        return (
            StatusCode::OK,
            Json(event.format_sync_reply(&greeting, &event.thread_name)),
        )
            .into_response();
    }

    if event.cleaned_text.is_empty() {
        return (StatusCode::OK, Json(json!({}))).into_response();
    }

    info!(
        sender = %event.sender_name,
        thread = %event.thread_name,
        message = %event.cleaned_text,
        "Received Google Chat message"
    );

    let has_async_channel = state.config.google_service_account_json.is_some()
        || state.config.google_chat_webhook_url.is_some();

    let agent = state.agent.clone();
    let thread_name = event.thread_name.clone();
    let sender_name = event.sender_name.clone();
    let user_text = event.cleaned_text.clone();

    if has_async_channel {
        // Wait up to 24 seconds for synchronous completion; if still running, acknowledge and finish asynchronously
        let (tx, rx) = tokio::sync::oneshot::channel();
        let state_clone = state.clone();
        let event_clone = event.clone();

        tokio::spawn(async move {
            let res = agent
                .handle_message(&thread_name, &sender_name, &user_text)
                .await;
            if let Err(res) = tx.send(res) {
                // Synchronous handler timed out and already returned an "on it!" ack; post final reply via async API
                let final_text = match res {
                    Ok(reply) => reply,
                    Err(e) => format!("⚠️ Sorry, I ran into an error: `{}`", e),
                };
                if let Err(post_err) = post_async_google_chat_message(
                    &state_clone.config,
                    &event_clone.space_name,
                    &event_clone.thread_name,
                    &final_text,
                )
                .await
                {
                    error!("Failed to post async Google Chat follow-up: {}", post_err);
                }
            }
        });

        match tokio::time::timeout(std::time::Duration::from_secs(24), rx).await {
            Ok(Ok(Ok(reply))) => (
                StatusCode::OK,
                Json(event.format_sync_reply(&reply, &event.thread_name)),
            )
                .into_response(),
            Ok(Ok(Err(e))) => {
                error!("Agent error: {}", e);
                let msg = format!("⚠️ Sorry, I encountered an error: `{}`", e);
                (
                    StatusCode::OK,
                    Json(event.format_sync_reply(&msg, &event.thread_name)),
                )
                    .into_response()
            }
            _ => {
                let ack = "👀 On it! Inspecting the repository / PR now—I'll post my findings in this thread in a moment...";
                (
                    StatusCode::OK,
                    Json(event.format_sync_reply(ack, &event.thread_name)),
                )
                    .into_response()
            }
        }
    } else {
        // Synchronous reply mode
        match agent
            .handle_message(&thread_name, &sender_name, &user_text)
            .await
        {
            Ok(reply) => (
                StatusCode::OK,
                Json(event.format_sync_reply(&reply, &event.thread_name)),
            )
                .into_response(),
            Err(e) => {
                error!("Agent error: {}", e);
                let msg = format!("⚠️ Sorry, I encountered an error: `{}`", e);
                (
                    StatusCode::OK,
                    Json(event.format_sync_reply(&msg, &event.thread_name)),
                )
                    .into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_mention() {
        assert_eq!(
            clean_mention(
                "@gemini, please review the pr https://github.com/Nafyaz/Homelab/pull/1",
                "gemini"
            ),
            "please review the pr https://github.com/Nafyaz/Homelab/pull/1"
        );
        assert_eq!(
            clean_mention("@Gemini: what changed in this PR?", "gemini"),
            "what changed in this PR?"
        );
    }

    #[test]
    fn test_parse_classic_google_chat_event() {
        let payload = json!({
            "type": "MESSAGE",
            "token": "secret",
            "space": { "name": "spaces/AAA" },
            "message": {
                "text": "@gemini, please review the pr https://github.com/Nafyaz/Homelab/pull/1",
                "sender": { "displayName": "Nafis Faiyaz" },
                "thread": { "name": "spaces/AAA/threads/BBB" }
            }
        });

        let ev = NormalizedChatEvent::from_payload(&payload, "gemini").unwrap();
        assert_eq!(ev.sender_name, "Nafis Faiyaz");
        assert_eq!(ev.space_name, "spaces/AAA");
        assert_eq!(ev.thread_name, "spaces/AAA/threads/BBB");
        assert_eq!(
            ev.cleaned_text,
            "please review the pr https://github.com/Nafyaz/Homelab/pull/1"
        );
        assert!(!ev.is_workspace_addon);
    }

    #[test]
    fn test_parse_workspace_addon_chat_event() {
        let payload = json!({
            "chat": {
                "user": { "displayName": "Nafis" },
                "messagePayload": {
                    "space": { "name": "spaces/XYZ" },
                    "message": {
                        "text": "@gemini please review https://github.com/Nafyaz/Homelab/pull/1",
                        "argumentText": " please review https://github.com/Nafyaz/Homelab/pull/1",
                        "thread": { "name": "spaces/XYZ/threads/123" }
                    }
                }
            }
        });

        let ev = NormalizedChatEvent::from_payload(&payload, "gemini").unwrap();
        assert!(ev.is_workspace_addon);
        assert_eq!(ev.sender_name, "Nafis");
        assert_eq!(ev.thread_name, "spaces/XYZ/threads/123");
        assert_eq!(
            ev.cleaned_text,
            "please review https://github.com/Nafyaz/Homelab/pull/1"
        );
    }
}
