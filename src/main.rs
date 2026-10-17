pub mod agent;
pub mod config;
pub mod db;
pub mod google_chat;
pub mod mcp;
pub mod openrouter;
pub mod tools;

use crate::agent::Agent;
use crate::config::Config;
use crate::db::ThreadStore;
use crate::google_chat::google_chat_webhook_handler;
use crate::mcp::McpManager;
use crate::openrouter::OpenRouterClient;
use crate::tools::github::GitHubTools;
use anyhow::Result;
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

pub struct AppState {
    pub config: Config,
    pub agent: Agent,
    pub mcp: McpManager,
}

async fn health_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "bot_name": state.config.bot_name,
        "model": state.config.openrouter_model,
        "github_mcp_connected": state.mcp.has_github_mcp(),
        "mcp_tools_loaded": state.mcp.tool_definitions().len(),
    }))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "chatni=info,axum=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Config::from_env()?;
    info!(
        bot_name = %config.bot_name,
        model = %config.openrouter_model,
        sqlite_path = %config.sqlite_path,
        "Starting Chatni server"
    );

    let db = ThreadStore::new(&config.sqlite_path)?;
    let openrouter = OpenRouterClient::new(&config);
    let mcp = McpManager::init(&config).await;
    let github_fallback = GitHubTools::new(config.github_token.clone());

    let agent = Agent::new(
        config.clone(),
        db,
        openrouter,
        mcp.clone(),
        github_fallback,
    );

    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    let state = Arc::new(AppState {
        config,
        agent,
        mcp,
    });

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/", post(google_chat_webhook_handler))
        .route("/webhook/google-chat", post(google_chat_webhook_handler))
        .with_state(state);

    info!("Listening on http://{}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
