use crate::config::Config;
use crate::openrouter::{FunctionDefinition, ToolDefinition};
use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tracing::{info, warn};

/// Standard `mcp.json` format (compatible with Claude Desktop / Cursor / VS Code)
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct McpConfigFile {
    #[serde(rename = "mcpServers", default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpServerConfig {
    /// For stdio MCP servers (e.g. `github-mcp-server` or `npx` or `docker`)
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// For remote Streamable-HTTP MCP servers (e.g. `https://api.githubcopilot.com/mcp/`)
    pub url: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
struct McpTool {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(rename = "inputSchema", default = "default_input_schema")]
    input_schema: Value,
}

fn default_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {}
    })
}

struct StdioSession {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl StdioSession {
    async fn spawn(
        server_name: &str,
        command: &str,
        args: &[String],
        env_vars: &HashMap<String, String>,
        fallback_github_token: Option<&str>,
    ) -> Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        if let Some(token) = fallback_github_token {
            cmd.env("GITHUB_PERSONAL_ACCESS_TOKEN", token);
            cmd.env("GITHUB_TOKEN", token);
        }

        for (k, v) in env_vars {
            let expanded = if v == "${GITHUB_TOKEN}" || v == "$GITHUB_TOKEN" {
                fallback_github_token.unwrap_or_default().to_string()
            } else {
                v.clone()
            };
            if !expanded.is_empty() {
                cmd.env(k, expanded);
            }
        }

        let mut child = cmd.spawn().with_context(|| {
            format!(
                "Failed to spawn MCP stdio server '{}' with command '{}'",
                server_name, command
            )
        })?;

        let stdin = child
            .stdin
            .take()
            .context("Failed to open stdin for MCP stdio server")?;
        let stdout = child
            .stdout
            .take()
            .context("Failed to open stdout for MCP stdio server")?;

        let mut session = Self {
            _child: child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        };

        session.initialize().await?;
        Ok(session)
    }

    async fn send_request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;

        let mut buf = String::new();
        loop {
            buf.clear();
            let bytes = tokio::time::timeout(
                std::time::Duration::from_secs(60),
                self.stdout.read_line(&mut buf),
            )
            .await
            .context("Timed out waiting for MCP stdio response")??;

            if bytes == 0 {
                anyhow::bail!("MCP stdio server closed stdout unexpectedly");
            }

            let trimmed = buf.trim();
            if trimmed.is_empty() {
                continue;
            }

            let msg: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if msg.get("id").and_then(|v| v.as_u64()) == Some(id) {
                if let Some(err) = msg.get("error") {
                    anyhow::bail!("MCP stdio error from {}: {}", method, err);
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    async fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        let notif = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        let mut line = serde_json::to_string(&notif)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn initialize(&mut self) -> Result<()> {
        let _ = self
            .send_request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "chatni",
                        "version": "0.1.0"
                    }
                }),
            )
            .await?;

        self.send_notification("notifications/initialized", json!({}))
            .await?;
        Ok(())
    }
}

struct HttpMcpSession {
    http: Client,
    url: String,
    headers: HashMap<String, String>,
    session_id: Mutex<Option<String>>,
    next_id: AtomicU64,
}

impl HttpMcpSession {
    async fn connect(
        url: String,
        mut headers: HashMap<String, String>,
        fallback_github_token: Option<&str>,
    ) -> Result<Self> {
        if !headers.contains_key("Authorization") && !headers.contains_key("authorization") {
            if let Some(token) = fallback_github_token {
                headers.insert("Authorization".to_string(), format!("Bearer {}", token));
            }
        }

        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;

        let session = Self {
            http,
            url,
            headers,
            session_id: Mutex::new(None),
            next_id: AtomicU64::new(1),
        };

        session.initialize().await?;
        Ok(session)
    }

    async fn initialize(&self) -> Result<()> {
        let _ = self
            .send_request(
                "initialize",
                json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "chatni",
                        "version": "0.1.0"
                    }
                }),
            )
            .await?;
        Ok(())
    }

    async fn send_request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        let mut req = self
            .http
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");

        for (k, v) in &self.headers {
            req = req.header(k, v);
        }

        if let Some(sid) = self.session_id.lock().await.clone() {
            req = req.header("Mcp-Session-Id", sid);
        }

        let resp = req.json(&payload).send().await?;
        let status = resp.status();

        if let Some(new_sid) = resp
            .headers()
            .get("Mcp-Session-Id")
            .or_else(|| resp.headers().get("mcp-session-id"))
            .and_then(|v| v.to_str().ok())
        {
            *self.session_id.lock().await = Some(new_sid.to_string());
        }

        let body_text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("HTTP MCP server error ({}): {}", status, body_text);
        }

        // Parse either direct JSON or SSE `data: {...}` lines
        let msg = parse_json_or_sse(&body_text)?;
        if let Some(err) = msg.get("error") {
            anyhow::bail!("HTTP MCP RPC error from {}: {}", method, err);
        }

        Ok(msg.get("result").cloned().unwrap_or(Value::Null))
    }
}

fn parse_json_or_sse(raw: &str) -> Result<Value> {
    let trimmed = raw.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return Ok(v);
    }

    for line in trimmed.lines().rev() {
        let line = line.trim();
        if let Some(data) = line.strip_prefix("data:") {
            let data = data.trim();
            if let Ok(v) = serde_json::from_str::<Value>(data) {
                if v.get("result").is_some() || v.get("error").is_some() {
                    return Ok(v);
                }
            }
        }
    }

    anyhow::bail!("Unable to parse MCP response as JSON or SSE: {}", trimmed)
}

enum McpBackend {
    Stdio(Mutex<StdioSession>),
    Http(HttpMcpSession),
}

impl McpBackend {
    async fn list_tools(&self) -> Result<Vec<McpTool>> {
        let res = match self {
            McpBackend::Stdio(s) => s.lock().await.send_request("tools/list", json!({})).await?,
            McpBackend::Http(h) => h.send_request("tools/list", json!({})).await?,
        };

        let tools_val = res.get("tools").cloned().unwrap_or_else(|| json!([]));
        let tools: Vec<McpTool> = serde_json::from_value(tools_val)?;
        Ok(tools)
    }

    async fn call_tool(&self, name: &str, arguments: Value) -> Result<String> {
        let res = match self {
            McpBackend::Stdio(s) => {
                s.lock()
                    .await
                    .send_request(
                        "tools/call",
                        json!({
                            "name": name,
                            "arguments": arguments
                        }),
                    )
                    .await?
            }
            McpBackend::Http(h) => {
                h.send_request(
                    "tools/call",
                    json!({
                        "name": name,
                        "arguments": arguments
                    }),
                )
                .await?
            }
        };

        Ok(format_mcp_tool_result(&res))
    }
}

fn format_mcp_tool_result(res: &Value) -> String {
    if let Some(content_arr) = res.get("content").and_then(|v| v.as_array()) {
        let mut parts = Vec::new();
        for item in content_arr {
            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                parts.push(text.to_string());
            } else {
                parts.push(item.to_string());
            }
        }
        if !parts.is_empty() {
            return parts.join("\n\n");
        }
    }
    serde_json::to_string_pretty(res).unwrap_or_else(|_| res.to_string())
}

#[derive(Clone)]
pub struct McpManager {
    /// Map of server_name -> backend
    backends: Arc<HashMap<String, Arc<McpBackend>>>,
    /// Map of OpenAI tool name -> (server_name, original_mcp_tool_name)
    tool_routes: Arc<HashMap<String, (String, String)>>,
    /// Cached OpenAI-compatible tool definitions discovered from MCP servers
    tool_definitions: Arc<Vec<ToolDefinition>>,
}

impl McpManager {
    pub async fn init(config: &Config) -> Self {
        let mut server_configs: HashMap<String, McpServerConfig> = HashMap::new();

        // 1. Load from mcp.json if present
        if Path::new(&config.mcp_config_path).exists() {
            match std::fs::read_to_string(&config.mcp_config_path) {
                Ok(raw) => match serde_json::from_str::<McpConfigFile>(&raw) {
                    Ok(parsed) => {
                        info!(
                            "Loaded {} MCP server config(s) from {}",
                            parsed.mcp_servers.len(),
                            config.mcp_config_path
                        );
                        server_configs.extend(parsed.mcp_servers);
                    }
                    Err(e) => warn!("Failed to parse {}: {}", config.mcp_config_path, e),
                },
                Err(e) => warn!("Failed to read {}: {}", config.mcp_config_path, e),
            }
        }

        // 2. Also allow quick configuration via GITHUB_MCP_URL or GITHUB_MCP_COMMAND env vars
        if !server_configs.contains_key("github") {
            if let Some(url) = &config.github_mcp_url {
                server_configs.insert(
                    "github".to_string(),
                    McpServerConfig {
                        command: None,
                        args: vec![],
                        env: HashMap::new(),
                        url: Some(url.clone()),
                        headers: HashMap::new(),
                    },
                );
            } else if let Some(cmd_line) = &config.github_mcp_command {
                let parts: Vec<String> = cmd_line
                    .split_whitespace()
                    .map(|s| s.to_string())
                    .collect();
                if let Some((first, rest)) = parts.split_first() {
                    server_configs.insert(
                        "github".to_string(),
                        McpServerConfig {
                            command: Some(first.clone()),
                            args: rest.to_vec(),
                            env: HashMap::new(),
                            url: None,
                            headers: HashMap::new(),
                        },
                    );
                }
            }
        }

        let mut backends = HashMap::new();
        let mut tool_routes = HashMap::new();
        let mut tool_definitions = Vec::new();

        for (server_name, srv_cfg) in server_configs {
            let backend_res: Result<McpBackend> = if let Some(url) = &srv_cfg.url {
                info!("Connecting to HTTP MCP server '{}' at {}", server_name, url);
                HttpMcpSession::connect(
                    url.clone(),
                    srv_cfg.headers.clone(),
                    config.github_token.as_deref(),
                )
                .await
                .map(McpBackend::Http)
            } else if let Some(command) = &srv_cfg.command {
                info!(
                    "Spawning stdio MCP server '{}' (`{} {}`)",
                    server_name,
                    command,
                    srv_cfg.args.join(" ")
                );
                StdioSession::spawn(
                    &server_name,
                    command,
                    &srv_cfg.args,
                    &srv_cfg.env,
                    config.github_token.as_deref(),
                )
                .await
                .map(|s| McpBackend::Stdio(Mutex::new(s)))
            } else {
                continue;
            };

            match backend_res {
                Ok(backend) => match backend.list_tools().await {
                    Ok(tools) => {
                        info!(
                            "Discovered {} tools from MCP server '{}'",
                            tools.len(),
                            server_name
                        );
                        let backend_arc = Arc::new(backend);
                        for t in tools {
                            let exposed_name = if tool_routes.contains_key(&t.name) {
                                format!("{}__{}", server_name, t.name)
                            } else {
                                t.name.clone()
                            };
                            tool_routes.insert(
                                exposed_name.clone(),
                                (server_name.clone(), t.name.clone()),
                            );
                            tool_definitions.push(ToolDefinition {
                                tool_type: "function".to_string(),
                                function: FunctionDefinition {
                                    name: exposed_name,
                                    description: t.description.unwrap_or_default(),
                                    parameters: t.input_schema,
                                },
                            });
                        }
                        backends.insert(server_name, backend_arc);
                    }
                    Err(e) => warn!(
                        "Connected to MCP server '{}' but failed to list tools: {}",
                        server_name, e
                    ),
                },
                Err(e) => warn!("Failed to initialize MCP server '{}': {}", server_name, e),
            }
        }

        Self {
            backends: Arc::new(backends),
            tool_routes: Arc::new(tool_routes),
            tool_definitions: Arc::new(tool_definitions),
        }
    }

    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        (*self.tool_definitions).clone()
    }

    pub fn has_tool(&self, tool_name: &str) -> bool {
        self.tool_routes.contains_key(tool_name)
    }

    pub fn has_github_mcp(&self) -> bool {
        self.backends.contains_key("github")
    }

    pub async fn call_tool(&self, tool_name: &str, args: Value) -> Result<String> {
        let (server_name, mcp_tool_name) = self
            .tool_routes
            .get(tool_name)
            .with_context(|| format!("Unknown MCP tool '{}'", tool_name))?;

        let backend = self
            .backends
            .get(server_name)
            .with_context(|| format!("MCP server '{}' not found", server_name))?;

        backend.call_tool(mcp_tool_name, args).await
    }
}
