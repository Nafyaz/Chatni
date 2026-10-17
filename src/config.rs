use anyhow::{Context, Result};
use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub sqlite_path: String,
    pub openrouter_api_key: String,
    pub openrouter_model: String,
    pub openrouter_base_url: String,
    pub github_token: Option<String>,
    /// Optional path to an `mcp.json` configuration file (Claude/Cursor-style `mcpServers` format)
    pub mcp_config_path: String,
    /// Optional remote HTTP MCP server URL (e.g. `https://api.githubcopilot.com/mcp/`)
    pub github_mcp_url: Option<String>,
    /// Optional stdio command to launch GitHub MCP server (e.g. `github-mcp-server stdio` or `npx -y @modelcontextprotocol/server-github`)
    pub github_mcp_command: Option<String>,
    pub bot_name: String,
    pub max_tool_iterations: usize,
    pub max_thread_messages: usize,
    /// Optional verification token from Google Cloud Console Chat App configuration
    pub google_chat_verification_token: Option<String>,
    /// Optional Google Service Account JSON (either file path or raw JSON content)
    /// Used to post asynchronous messages via the Google Chat REST API when tasks take long
    pub google_service_account_json: Option<String>,
    /// Optional Google Chat Space incoming webhook URL (fallback/alternative for async posts)
    pub google_chat_webhook_url: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let _ = dotenvy::dotenv();

        let host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
        let port = env::var("PORT")
            .unwrap_or_else(|_| "8080".to_string())
            .parse::<u16>()
            .context("PORT must be a valid u16 integer")?;

        let sqlite_path = env::var("SQLITE_PATH").unwrap_or_else(|_| "chatni.db".to_string());

        let openrouter_api_key = env::var("OPENROUTER_API_KEY")
            .context("OPENROUTER_API_KEY environment variable is required")?;

        let openrouter_model = env::var("OPENROUTER_MODEL")
            .unwrap_or_else(|_| "google/gemini-2.5-pro".to_string());

        let openrouter_base_url = env::var("OPENROUTER_BASE_URL")
            .unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_string());

        let github_token = env::var("GITHUB_TOKEN")
            .or_else(|_| env::var("GITHUB_PERSONAL_ACCESS_TOKEN"))
            .ok()
            .filter(|s| !s.trim().is_empty());

        let mcp_config_path =
            env::var("MCP_CONFIG_PATH").unwrap_or_else(|_| "mcp.json".to_string());

        let github_mcp_url = env::var("GITHUB_MCP_URL")
            .ok()
            .filter(|s| !s.trim().is_empty());

        let github_mcp_command = env::var("GITHUB_MCP_COMMAND")
            .ok()
            .filter(|s| !s.trim().is_empty());

        let bot_name = env::var("BOT_NAME").unwrap_or_else(|_| "gemini".to_string());

        let max_tool_iterations = env::var("MAX_TOOL_ITERATIONS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(10);

        let max_thread_messages = env::var("MAX_THREAD_MESSAGES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(30);

        let google_chat_verification_token = env::var("GOOGLE_CHAT_VERIFICATION_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty());

        let google_service_account_json = env::var("GOOGLE_SERVICE_ACCOUNT_JSON")
            .ok()
            .filter(|s| !s.trim().is_empty());

        let google_chat_webhook_url = env::var("GOOGLE_CHAT_WEBHOOK_URL")
            .ok()
            .filter(|s| !s.trim().is_empty());

        Ok(Self {
            host,
            port,
            sqlite_path,
            openrouter_api_key,
            openrouter_model,
            openrouter_base_url,
            github_token,
            mcp_config_path,
            github_mcp_url,
            github_mcp_command,
            bot_name,
            max_tool_iterations,
            max_thread_messages,
            google_chat_verification_token,
            google_service_account_json,
            google_chat_webhook_url,
        })
    }
}
