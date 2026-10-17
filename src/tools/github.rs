use crate::openrouter::{FunctionDefinition, ToolDefinition};
use anyhow::{Context, Result};
use base64::Engine;
use reqwest::{header, Client};
use serde_json::{json, Value};

const MAX_DIFF_CHARS: usize = 60_000;
const MAX_FILE_CHARS: usize = 40_000;

#[derive(Clone)]
pub struct GitHubTools {
    http: Client,
    token: Option<String>,
}

impl GitHubTools {
    pub fn new(token: Option<String>) -> Self {
        let http = Client::builder()
            .user_agent("chatni-google-chat-ai/0.1.0")
            .timeout(std::time::Duration::from_secs(45))
            .build()
            .expect("Failed to build GitHub HTTP client");

        Self { http, token }
    }

    fn request(&self, method: reqwest::Method, url: &str, accept: &str) -> reqwest::RequestBuilder {
        let mut req = self
            .http
            .request(method, url)
            .header(header::ACCEPT, accept)
            .header("X-GitHub-Api-Version", "2022-11-28");

        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        req
    }

    pub fn definitions() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "github_get_pr".to_string(),
                    description: "Fetch GitHub Pull Request metadata, description, author, branch refs (head/base SHA), and the list of changed files with additions/deletions.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "owner": { "type": "string", "description": "Repository owner/org, e.g. 'Nafyaz'" },
                            "repo": { "type": "string", "description": "Repository name, e.g. 'Homelab'" },
                            "pull_number": { "type": "integer", "description": "Pull request number, e.g. 1" }
                        },
                        "required": ["owner", "repo", "pull_number"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "github_get_pr_diff".to_string(),
                    description: "Fetch the unified git diff/patch of a GitHub Pull Request so you can review line-by-line code changes.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "owner": { "type": "string", "description": "Repository owner/org" },
                            "repo": { "type": "string", "description": "Repository name" },
                            "pull_number": { "type": "integer", "description": "Pull request number" }
                        },
                        "required": ["owner", "repo", "pull_number"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "github_get_file_content".to_string(),
                    description: "Fetch the full contents of a file in a GitHub repository at a given branch, tag, or commit SHA.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "owner": { "type": "string", "description": "Repository owner/org" },
                            "repo": { "type": "string", "description": "Repository name" },
                            "path": { "type": "string", "description": "File path inside the repository" },
                            "git_ref": { "type": "string", "description": "Optional branch name or commit SHA (defaults to repo default branch)" }
                        },
                        "required": ["owner", "repo", "path"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "github_list_pr_comments".to_string(),
                    description: "List existing discussion and inline code review comments on a GitHub Pull Request.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "owner": { "type": "string", "description": "Repository owner/org" },
                            "repo": { "type": "string", "description": "Repository name" },
                            "pull_number": { "type": "integer", "description": "Pull request number" }
                        },
                        "required": ["owner", "repo", "pull_number"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "github_submit_pr_review".to_string(),
                    description: "Submit an official Pull Request review on GitHub (COMMENT, APPROVE, or REQUEST_CHANGES) with a summary body and optional inline file comments.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "owner": { "type": "string", "description": "Repository owner/org" },
                            "repo": { "type": "string", "description": "Repository name" },
                            "pull_number": { "type": "integer", "description": "Pull request number" },
                            "event": {
                                "type": "string",
                                "enum": ["COMMENT", "APPROVE", "REQUEST_CHANGES"],
                                "description": "Review action type"
                            },
                            "body": { "type": "string", "description": "Markdown review summary body" },
                            "comments": {
                                "type": "array",
                                "description": "Optional inline review comments on specific files/lines",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "path": { "type": "string", "description": "Relative file path" },
                                        "line": { "type": "integer", "description": "Line number in the diff/file" },
                                        "body": { "type": "string", "description": "Inline comment text" }
                                    },
                                    "required": ["path", "line", "body"]
                                }
                            }
                        },
                        "required": ["owner", "repo", "pull_number", "event", "body"]
                    }),
                },
            },
        ]
    }

    pub async fn execute(&self, name: &str, args: &Value) -> Result<String> {
        match name {
            "github_get_pr" => self.get_pr(args).await,
            "github_get_pr_diff" => self.get_pr_diff(args).await,
            "github_get_file_content" => self.get_file_content(args).await,
            "github_list_pr_comments" => self.list_pr_comments(args).await,
            "github_submit_pr_review" => self.submit_pr_review(args).await,
            _ => anyhow::bail!("Unknown built-in GitHub tool: {}", name),
        }
    }

    async fn get_pr(&self, args: &Value) -> Result<String> {
        let owner = req_str(args, "owner")?;
        let repo = req_str(args, "repo")?;
        let pull_number = req_u64(args, "pull_number")?;

        let pr_url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}",
            owner, repo, pull_number
        );
        let files_url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}/files?per_page=100",
            owner, repo, pull_number
        );

        let pr_resp = self
            .request(reqwest::Method::GET, &pr_url, "application/vnd.github+json")
            .send()
            .await?;
        let pr_status = pr_resp.status();
        let pr_json: Value = pr_resp.json().await?;
        if !pr_status.is_success() {
            anyhow::bail!("GitHub API error ({}): {}", pr_status, pr_json);
        }

        let files_resp = self
            .request(
                reqwest::Method::GET,
                &files_url,
                "application/vnd.github+json",
            )
            .send()
            .await?;
        let files_json: Value = files_resp.json().await.unwrap_or_else(|_| json!([]));

        let files_summary: Vec<Value> = files_json
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|f| {
                        json!({
                            "filename": f.get("filename"),
                            "status": f.get("status"),
                            "additions": f.get("additions"),
                            "deletions": f.get("deletions"),
                            "changes": f.get("changes"),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let summary = json!({
            "number": pr_json.get("number"),
            "title": pr_json.get("title"),
            "state": pr_json.get("state"),
            "draft": pr_json.get("draft"),
            "user": pr_json.get("user").and_then(|u| u.get("login")),
            "body": pr_json.get("body"),
            "html_url": pr_json.get("html_url"),
            "created_at": pr_json.get("created_at"),
            "updated_at": pr_json.get("updated_at"),
            "head": {
                "ref": pr_json.get("head").and_then(|h| h.get("ref")),
                "sha": pr_json.get("head").and_then(|h| h.get("sha")),
            },
            "base": {
                "ref": pr_json.get("base").and_then(|b| b.get("ref")),
                "sha": pr_json.get("base").and_then(|b| b.get("sha")),
            },
            "additions": pr_json.get("additions"),
            "deletions": pr_json.get("deletions"),
            "changed_files_count": pr_json.get("changed_files"),
            "changed_files": files_summary,
        });

        Ok(serde_json::to_string_pretty(&summary)?)
    }

    async fn get_pr_diff(&self, args: &Value) -> Result<String> {
        let owner = req_str(args, "owner")?;
        let repo = req_str(args, "repo")?;
        let pull_number = req_u64(args, "pull_number")?;

        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}",
            owner, repo, pull_number
        );

        let resp = self
            .request(
                reqwest::Method::GET,
                &url,
                "application/vnd.github.v3.diff",
            )
            .send()
            .await?;

        let status = resp.status();
        let diff_text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("GitHub diff API error ({}): {}", status, diff_text);
        }

        if diff_text.len() > MAX_DIFF_CHARS {
            let truncated: String = diff_text.chars().take(MAX_DIFF_CHARS).collect();
            Ok(format!(
                "{}\n\n... [Diff truncated at {} chars out of {} total chars]",
                truncated,
                MAX_DIFF_CHARS,
                diff_text.len()
            ))
        } else {
            Ok(diff_text)
        }
    }

    async fn get_file_content(&self, args: &Value) -> Result<String> {
        let owner = req_str(args, "owner")?;
        let repo = req_str(args, "repo")?;
        let path = req_str(args, "path")?;
        let git_ref = args.get("git_ref").and_then(|v| v.as_str());

        let mut url = format!(
            "https://api.github.com/repos/{}/{}/contents/{}",
            owner,
            repo,
            path.trim_start_matches('/')
        );
        if let Some(r) = git_ref {
            if !r.trim().is_empty() {
                url.push_str(&format!("?ref={}", urlencoding::encode(r)));
            }
        }

        let resp = self
            .request(reqwest::Method::GET, &url, "application/vnd.github+json")
            .send()
            .await?;
        let status = resp.status();
        let body: Value = resp.json().await?;
        if !status.is_success() {
            anyhow::bail!("GitHub contents API error ({}): {}", status, body);
        }

        if let Some(encoded) = body.get("content").and_then(|c| c.as_str()) {
            let cleaned: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&cleaned)
                .context("Failed to decode base64 file content from GitHub")?;
            let text = String::from_utf8_lossy(&bytes).to_string();
            if text.len() > MAX_FILE_CHARS {
                let truncated: String = text.chars().take(MAX_FILE_CHARS).collect();
                return Ok(format!(
                    "{}\n\n... [File truncated at {} chars]",
                    truncated, MAX_FILE_CHARS
                ));
            }
            return Ok(text);
        }

        Ok(serde_json::to_string_pretty(&body)?)
    }

    async fn list_pr_comments(&self, args: &Value) -> Result<String> {
        let owner = req_str(args, "owner")?;
        let repo = req_str(args, "repo")?;
        let pull_number = req_u64(args, "pull_number")?;

        let issue_comments_url = format!(
            "https://api.github.com/repos/{}/{}/issues/{}/comments?per_page=50",
            owner, repo, pull_number
        );
        let review_comments_url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}/comments?per_page=50",
            owner, repo, pull_number
        );

        let issue_resp = self
            .request(
                reqwest::Method::GET,
                &issue_comments_url,
                "application/vnd.github+json",
            )
            .send()
            .await?;
        let issue_comments: Value = issue_resp.json().await.unwrap_or_else(|_| json!([]));

        let review_resp = self
            .request(
                reqwest::Method::GET,
                &review_comments_url,
                "application/vnd.github+json",
            )
            .send()
            .await?;
        let review_comments: Value = review_resp.json().await.unwrap_or_else(|_| json!([]));

        let result = json!({
            "issue_comments": issue_comments,
            "review_comments": review_comments,
        });

        Ok(serde_json::to_string_pretty(&result)?)
    }

    async fn submit_pr_review(&self, args: &Value) -> Result<String> {
        if self.token.is_none() {
            anyhow::bail!("GITHUB_TOKEN is not configured; cannot submit a PR review to GitHub.");
        }

        let owner = req_str(args, "owner")?;
        let repo = req_str(args, "repo")?;
        let pull_number = req_u64(args, "pull_number")?;
        let event = req_str(args, "event")?;
        let body = req_str(args, "body")?;

        let mut payload = json!({
            "event": event,
            "body": body,
        });

        if let Some(comments) = args.get("comments").and_then(|c| c.as_array()) {
            let formatted_comments: Vec<Value> = comments
                .iter()
                .filter_map(|c| {
                    Some(json!({
                        "path": c.get("path")?.as_str()?,
                        "line": c.get("line")?.as_u64()?,
                        "side": "RIGHT",
                        "body": c.get("body")?.as_str()?,
                    }))
                })
                .collect();
            if !formatted_comments.is_empty() {
                payload["comments"] = Value::Array(formatted_comments);
            }
        }

        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}/reviews",
            owner, repo, pull_number
        );

        let resp = self
            .request(reqwest::Method::POST, &url, "application/vnd.github+json")
            .json(&payload)
            .send()
            .await?;

        let status = resp.status();
        let resp_json: Value = resp.json().await?;
        if !status.is_success() {
            anyhow::bail!(
                "Failed to submit GitHub PR review ({}): {}",
                status,
                resp_json
            );
        }

        Ok(serde_json::to_string_pretty(&json!({
            "status": "submitted",
            "id": resp_json.get("id"),
            "state": resp_json.get("state"),
            "html_url": resp_json.get("html_url"),
        }))?)
    }
}

fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .with_context(|| format!("Missing required string parameter '{}'", key))
}

fn req_u64(args: &Value, key: &str) -> Result<u64> {
    if let Some(n) = args.get(key).and_then(|v| v.as_u64()) {
        return Ok(n);
    }
    if let Some(s) = args.get(key).and_then(|v| v.as_str()) {
        if let Ok(n) = s.parse::<u64>() {
            return Ok(n);
        }
    }
    anyhow::bail!("Missing or invalid integer parameter '{}'", key)
}
