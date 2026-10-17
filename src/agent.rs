use crate::config::Config;
use crate::db::ThreadStore;
use crate::mcp::McpManager;
use crate::openrouter::{ChatMessage, OpenRouterClient, ToolDefinition};
use crate::tools::github::GitHubTools;
use anyhow::Result;
use serde_json::{json, Value};
use tracing::{info, warn};

#[derive(Clone)]
pub struct Agent {
    config: Config,
    db: ThreadStore,
    openrouter: OpenRouterClient,
    mcp: McpManager,
    github_fallback: GitHubTools,
}

impl Agent {
    pub fn new(
        config: Config,
        db: ThreadStore,
        openrouter: OpenRouterClient,
        mcp: McpManager,
        github_fallback: GitHubTools,
    ) -> Self {
        Self {
            config,
            db,
            openrouter,
            mcp,
            github_fallback,
        }
    }

    fn system_prompt(&self) -> String {
        format!(
            "You are @{bot_name}, a sharp, collaborative senior software engineer and remote teammate in Google Chat.\n\
             \n\
             Guidelines:\n\
             - Talk naturally like a helpful teammate in chat: direct, constructive, and concise.\n\
             - When someone asks you to review a GitHub Pull Request (e.g. `https://github.com/<owner>/<repo>/pull/<number>`), \
               parse `owner`, `repo`, and `pull_number` from the URL and immediately use your GitHub tools (MCP tools or `github_get_pr` / `github_get_pr_diff` / `github_get_file_content`) to inspect the PR metadata, changed files, and actual diff before answering.\n\
             - In PR reviews, highlight:\n\
               1. High-level summary of what the PR does.\n\
               2. Bugs, security issues, race conditions, or edge cases (referencing exact file paths and line numbers).\n\
               3. Concrete suggestions for improvement.\n\
               4. Clear verdict (Looks good to merge / Needs minor tweaks / Request changes).\n\
             - Only submit a formal review directly onto GitHub (`create_pull_request_review` or `github_submit_pr_review`) if the teammate explicitly asks you to post/submit/approve/comment on GitHub, OR if they ask you to leave the review on the PR.\n\
             - Format responses cleanly for Google Chat (use `*bold*`, `` `inline code` ``, and ```code blocks```).",
            bot_name = self.config.bot_name
        )
    }

    fn available_tools(&self) -> Vec<ToolDefinition> {
        let mut tools = self.mcp.tool_definitions();
        let builtin = GitHubTools::definitions();

        if !self.mcp.has_github_mcp() {
            tools.extend(builtin);
        } else {
            // Even when GitHub MCP is active, keep `github_get_pr_diff` available if the MCP server
            // doesn't expose a unified diff tool by name.
            for b in builtin {
                if !self.mcp.has_tool(&b.function.name) && b.function.name == "github_get_pr_diff" {
                    tools.push(b);
                }
            }
        }
        tools
    }

    pub async fn handle_message(
        &self,
        thread_id: &str,
        sender_name: &str,
        user_text: &str,
    ) -> Result<String> {
        let formatted_user_msg = format!("[{}]: {}", sender_name, user_text);
        let user_msg = ChatMessage::user(formatted_user_msg);

        // Load existing conversation history for this Google Chat thread
        let mut history = self
            .db
            .load_thread_history(thread_id, self.config.max_thread_messages)
            .unwrap_or_default();

        // Persist incoming user message
        if let Err(e) = self
            .db
            .append_message(thread_id, Some(sender_name), &user_msg)
        {
            warn!("Failed to persist user message to SQLite: {}", e);
        }
        history.push(user_msg);

        let mut messages = Vec::with_capacity(history.len() + 1);
        messages.push(ChatMessage::system(self.system_prompt()));
        messages.extend(history);

        let tools = self.available_tools();

        for iteration in 0..self.config.max_tool_iterations {
            info!(
                thread_id = %thread_id,
                iteration = iteration + 1,
                tool_count = tools.len(),
                "Calling OpenRouter chat completion"
            );

            let assistant_msg = self
                .openrouter
                .chat_completion(&messages, tools.clone())
                .await?;

            let has_tool_calls = assistant_msg
                .tool_calls
                .as_ref()
                .map(|tc| !tc.is_empty())
                .unwrap_or(false);

            if !has_tool_calls {
                let final_text = assistant_msg
                    .content
                    .clone()
                    .unwrap_or_else(|| "Done.".to_string());

                let stored_assistant = ChatMessage::assistant(Some(final_text.clone()), None);
                if let Err(e) = self.db.append_message(thread_id, None, &stored_assistant) {
                    warn!("Failed to persist assistant message to SQLite: {}", e);
                }
                return Ok(final_text);
            }

            let tool_calls = assistant_msg.tool_calls.clone().unwrap_or_default();
            messages.push(assistant_msg);

            for tc in tool_calls {
                let tool_name = &tc.function.name;
                let parsed_args: Value =
                    serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| json!({}));

                info!(
                    thread_id = %thread_id,
                    tool = %tool_name,
                    "Executing tool call"
                );

                let tool_output = if self.mcp.has_tool(tool_name) {
                    match self.mcp.call_tool(tool_name, parsed_args).await {
                        Ok(out) => out,
                        Err(e) => format!("Error executing MCP tool `{}`: {}", tool_name, e),
                    }
                } else {
                    match self.github_fallback.execute(tool_name, &parsed_args).await {
                        Ok(out) => out,
                        Err(e) => format!("Error executing tool `{}`: {}", tool_name, e),
                    }
                };

                messages.push(ChatMessage::tool(tc.id, tool_name.clone(), tool_output));
            }
        }

        // If we hit max tool iterations, ask the model for a final synthesis without tools
        let final_msg = self.openrouter.chat_completion(&messages, vec![]).await?;
        let text = final_msg
            .content
            .unwrap_or_else(|| "Reached maximum tool iterations.".to_string());

        let stored_assistant = ChatMessage::assistant(Some(text.clone()), None);
        let _ = self.db.append_message(thread_id, None, &stored_assistant);

        Ok(text)
    }
}
