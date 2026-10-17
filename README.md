# Chatni — Your AI Engineering Teammate in Google Chat (Rust + Axum + OpenRouter + GitHub MCP)

Talk to AI in Google Chat just like you talk to your remote teammates:
> **You:** `@gemini, please review the pr https://github.com/Nafyaz/Homelab/pull/1`
> **@gemini:** *Fetches the PR metadata, changed files, and unified diff via GitHub MCP, analyzes the changes, and replies in the same Google Chat thread (or posts the review directly on GitHub if you ask).*

---

## Features

- **Rust + Axum Web Server**: Fast, lightweight HTTP server handling Google Chat webhook events (`POST /webhook/google-chat` or `POST /`).
- **OpenRouter Integration**: Works with any tool-calling model on OpenRouter (`google/gemini-2.5-pro`, `anthropic/claude-3.7-sonnet`, `openai/gpt-4o`, etc.).
- **Native MCP Client (`src/mcp.rs`)**:
  - Connects to **GitHub's official remote HTTP MCP server** (`https://api.githubcopilot.com/mcp/`) or spawns the **local stdio `github-mcp-server`** (`github-mcp-server stdio`, Docker, or `npx`).
  - Supports standard `mcp.json` (`mcpServers` format) so you can plug in additional Homelab MCP servers (Kubernetes, Grafana, Proxmox, Filesystem, etc.).
  - Includes built-in GitHub REST fallback tools (`github_get_pr`, `github_get_pr_diff`, `github_get_file_content`, `github_list_pr_comments`, `github_submit_pr_review`) so PR diffs and reviews always work out of the box.
- **Per-Thread SQLite Memory (`src/db.rs`)**: Remembers the conversation history inside each Google Chat thread (`spaces/{space}/threads/{thread}`), allowing natural follow-up questions.
- **Synchronous & Asynchronous Google Chat Replies**: Replies synchronously within 24s, or acknowledges immediately and posts follow-up messages via Google Chat API / Space Webhook for longer multi-step reviews.

---

## Quick Start

### 1. Configure Environment Variables

Copy `.env.example` to `.env`:

```bash
cp .env.example .env
```

Edit `.env` and set:
- `OPENROUTER_API_KEY`: Your OpenRouter API key.
- `OPENROUTER_MODEL`: e.g., `google/gemini-2.5-pro`
- `GITHUB_TOKEN`: A GitHub Personal Access Token (with `repo` / `pull_requests` permissions).

### 2. Configure GitHub MCP (`mcp.json`)

By default, [`mcp.json`](mcp.json) connects to GitHub's official remote Streamable-HTTP MCP server using your `GITHUB_TOKEN`:

```json
{
  "mcpServers": {
    "github": {
      "url": "https://api.githubcopilot.com/mcp/"
    }
  }
}
```

Prefer running the local stdio `github-mcp-server` binary or Docker container? You can either set `GITHUB_MCP_COMMAND="github-mcp-server stdio"` in `.env` or update `mcp.json`:

```json
{
  "mcpServers": {
    "github": {
      "command": "github-mcp-server",
      "args": ["stdio"],
      "env": {
        "GITHUB_PERSONAL_ACCESS_TOKEN": "${GITHUB_TOKEN}"
      }
    }
  }
}
```

### 3. Run Locally

```bash
cargo run --release
```

### 4. Test Locally with `curl`

```bash
curl -X POST http://localhost:8080/webhook/google-chat \
  -H "Content-Type: application/json" \
  -d '{
    "type": "MESSAGE",
    "space": { "name": "spaces/homelab" },
    "message": {
      "text": "@gemini, please review the pr https://github.com/Nafyaz/Homelab/pull/1",
      "sender": { "displayName": "Nafis" },
      "thread": { "name": "spaces/homelab/threads/pr-1" }
    }
  }'
```

---

## Connecting to Google Chat

1. Expose your Chatni server over HTTPS (e.g., via **Cloudflare Tunnel**, **Tailscale Funnel**, or reverse proxy in your Homelab) so it has a public URL like `https://chatni.yourdomain.com/webhook/google-chat`.
2. Open the [Google Cloud Console](https://console.cloud.google.com/) -> **APIs & Services** -> Enable **Google Chat API**.
3. Go to **Google Chat API** -> **Configuration**:
   - **App name**: `gemini` (or whatever matches `BOT_NAME` in your `.env`)
   - **Avatar URL**: Any icon URL you like
   - **Description**: `AI Engineering Teammate`
   - **Interactive features**: Enable **Receive 1:1 messages** and **Join spaces and group conversations**
   - **Connection settings**: Select **HTTP endpoint URL** and enter `https://chatni.yourdomain.com/webhook/google-chat`
   - **Visibility**: Make it available to yourself or your Google Workspace domain.
4. Add `@gemini` to any Google Chat space and mention it:
   `@gemini, please review the pr https://github.com/Nafyaz/Homelab/pull/1`
