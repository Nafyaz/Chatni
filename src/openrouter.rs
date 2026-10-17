use crate::config::Config;
use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: Some(content.into()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: Some(content.into()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant(content: Option<String>, tool_calls: Option<Vec<ToolCall>>) -> Self {
        Self {
            role: "assistant".to_string(),
            content,
            name: None,
            tool_calls,
            tool_call_id: None,
        }
    }

    pub fn tool(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            role: "tool".to_string(),
            content: Some(content.into()),
            name: Some(name.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "default_tool_type")]
    pub tool_type: String,
    pub function: FunctionCall,
}

fn default_tool_type() -> String {
    "function".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDefinition,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ToolDefinition>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Clone)]
pub struct OpenRouterClient {
    http: Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl OpenRouterClient {
    pub fn new(config: &Config) -> Self {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("Failed to build reqwest client");

        Self {
            http,
            api_key: config.openrouter_api_key.clone(),
            model: config.openrouter_model.clone(),
            base_url: config.openrouter_base_url.trim_end_matches('/').to_string(),
        }
    }

    pub async fn chat_completion(
        &self,
        messages: &[ChatMessage],
        tools: Vec<ToolDefinition>,
    ) -> Result<ChatMessage> {
        let url = format!("{}/chat/completions", self.base_url);
        let req_body = ChatCompletionRequest {
            model: &self.model,
            messages,
            tools,
        };

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", "https://github.com/Nafyaz/Chatni")
            .header("X-Title", "Chatni - Google Chat AI Teammate")
            .json(&req_body)
            .send()
            .await
            .context("Failed to send request to OpenRouter")?;

        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            anyhow::bail!("OpenRouter API error ({}): {}", status, body_text);
        }

        let parsed: ChatCompletionResponse = serde_json::from_str(&body_text)
            .with_context(|| format!("Failed to parse OpenRouter response: {}", body_text))?;

        parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .context("OpenRouter returned empty choices")
    }
}
