use crate::openrouter::ChatMessage;
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct ThreadStore {
    conn: Arc<Mutex<Connection>>,
}

impl ThreadStore {
    pub fn new(db_path: &str) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open SQLite database at {}", db_path))?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS thread_messages (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 thread_id TEXT NOT NULL,
                 sender_name TEXT,
                 message_json TEXT NOT NULL,
                 created_at DATETIME DEFAULT CURRENT_TIMESTAMP
             );
             CREATE INDEX IF NOT EXISTS idx_thread_messages_thread_id
                 ON thread_messages(thread_id, id);",
        )
        .context("Failed to initialize SQLite schema")?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn append_message(
        &self,
        thread_id: &str,
        sender_name: Option<&str>,
        message: &ChatMessage,
    ) -> Result<()> {
        let json = serde_json::to_string(message)?;
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Mutex poisoned: {}", e))?;

        conn.execute(
            "INSERT INTO thread_messages (thread_id, sender_name, message_json) VALUES (?1, ?2, ?3)",
            params![thread_id, sender_name, json],
        )?;
        Ok(())
    }

    pub fn load_thread_history(&self, thread_id: &str, limit: usize) -> Result<Vec<ChatMessage>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Mutex poisoned: {}", e))?;

        let mut stmt = conn.prepare(
            "SELECT message_json FROM (
                 SELECT id, message_json FROM thread_messages
                 WHERE thread_id = ?1
                 ORDER BY id DESC
                 LIMIT ?2
             ) ORDER BY id ASC",
        )?;

        let rows = stmt.query_map(params![thread_id, limit as i64], |row| {
            let json_str: String = row.get(0)?;
            Ok(json_str)
        })?;

        let mut messages = Vec::new();
        for row in rows {
            let json_str = row?;
            if let Ok(msg) = serde_json::from_str::<ChatMessage>(&json_str) {
                messages.push(msg);
            }
        }
        Ok(messages)
    }
}
