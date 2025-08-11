use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqlitePool, Row};
use crate::types::{Role, Metadata};
use anyhow::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseSession {
    pub id: String,
    pub object: String,
    pub created_at: i64,
    pub status: String,
    pub model: String,
    pub previous_response_id: Option<String>,
    pub instructions: Option<String>,
    pub max_output_tokens: Option<i64>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub store: bool,
    pub metadata: Option<Metadata>,
    pub user_id: Option<String>,
    pub safety_identifier: Option<String>,
    pub prompt_cache_key: Option<String>,
    pub usage_input_tokens: Option<i64>,
    pub usage_output_tokens: Option<i64>,
    pub usage_total_tokens: Option<i64>,
    pub error: Option<String>, // JSON string if error occurred
    pub incomplete_details: Option<String>, // JSON string
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputItem {
    pub id: String,
    pub response_id: String,
    pub item_type: String, // "message", "file", etc.
    pub role: Option<Role>,
    pub content: String, // JSON string
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputItem {
    pub id: String,
    pub response_id: String,
    pub item_type: String, // "message", "tool_call", etc.
    pub role: Option<Role>,
    pub content: String, // JSON string
    pub status: String,
    pub created_at: i64,
}

pub struct DatabaseManager {
    pub pool: SqlitePool,
}

impl DatabaseManager {
    pub async fn new(database_path: &str) -> Result<Self> {
        // Ensure parent directory exists if path contains one
        if let Some(parent) = std::path::Path::new(database_path).parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        // Create database URL with proper mode
        let database_url = if database_path.starts_with("sqlite:") || database_path.starts_with("file:") {
            database_path.to_string()
        } else {
            format!("sqlite:{}?mode=rwc", database_path)
        };

        let pool = SqlitePool::connect(&database_url).await?;
        
        let manager = Self { pool };
        manager.initialize_tables().await?;
        Ok(manager)
    }

    pub async fn initialize_tables(&self) -> Result<()> {
        // Create responses table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS responses (
                id TEXT PRIMARY KEY,
                object TEXT NOT NULL DEFAULT 'response',
                created_at INTEGER NOT NULL,
                status TEXT NOT NULL,
                model TEXT NOT NULL,
                previous_response_id TEXT,
                instructions TEXT,
                max_output_tokens INTEGER,
                temperature REAL,
                top_p REAL,
                store BOOLEAN NOT NULL DEFAULT TRUE,
                metadata TEXT,
                user_id TEXT,
                safety_identifier TEXT,
                prompt_cache_key TEXT,
                usage_input_tokens INTEGER,
                usage_output_tokens INTEGER,
                usage_total_tokens INTEGER,
                error TEXT,
                incomplete_details TEXT,
                FOREIGN KEY (previous_response_id) REFERENCES responses(id)
            )
            "#
        )
        .execute(&self.pool)
        .await?;

        // Create input_items table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS input_items (
                id TEXT PRIMARY KEY,
                response_id TEXT NOT NULL,
                item_type TEXT NOT NULL,
                role TEXT,
                content TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                FOREIGN KEY (response_id) REFERENCES responses(id) ON DELETE CASCADE
            )
            "#
        )
        .execute(&self.pool)
        .await?;

        // Create output_items table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS output_items (
                id TEXT PRIMARY KEY,
                response_id TEXT NOT NULL,
                item_type TEXT NOT NULL,
                role TEXT,
                content TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                FOREIGN KEY (response_id) REFERENCES responses(id) ON DELETE CASCADE
            )
            "#
        )
        .execute(&self.pool)
        .await?;

        // Create indexes for better query performance
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_responses_previous_id ON responses(previous_response_id)")
            .execute(&self.pool)
            .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_responses_user_id ON responses(user_id)")
            .execute(&self.pool)
            .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_responses_created_at ON responses(created_at)")
            .execute(&self.pool)
            .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_input_items_response_id ON input_items(response_id)")
            .execute(&self.pool)
            .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_output_items_response_id ON output_items(response_id)")
            .execute(&self.pool)
            .await?;
        
        Ok(())
    }

    pub async fn store_response(&self, response: ResponseSession) -> Result<()> {
        // Convert metadata to JSON string if present
        let metadata_json = response.metadata.as_ref()
            .map(|m| serde_json::to_string(m))
            .transpose()?;

        sqlx::query(
            r#"
            INSERT INTO responses (
                id, object, created_at, status, model, previous_response_id, 
                instructions, max_output_tokens, temperature, top_p, store, 
                metadata, user_id, safety_identifier, prompt_cache_key,
                usage_input_tokens, usage_output_tokens, usage_total_tokens,
                error, incomplete_details
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
            "#
        )
        .bind(response.id)
        .bind(response.object)
        .bind(response.created_at)
        .bind(response.status)
        .bind(response.model)
        .bind(response.previous_response_id)
        .bind(response.instructions)
        .bind(response.max_output_tokens)
        .bind(response.temperature)
        .bind(response.top_p)
        .bind(response.store)
        .bind(metadata_json)
        .bind(response.user_id)
        .bind(response.safety_identifier)
        .bind(response.prompt_cache_key)
        .bind(response.usage_input_tokens)
        .bind(response.usage_output_tokens)
        .bind(response.usage_total_tokens)
        .bind(response.error)
        .bind(response.incomplete_details)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn store_input_item(&self, item: InputItem) -> Result<()> {
        // Convert role to string if present
        let role_str = item.role.as_ref().map(|r| r.to_string());

        sqlx::query(
            r#"
            INSERT INTO input_items (id, response_id, item_type, role, content, created_at) 
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#
        )
        .bind(item.id)
        .bind(item.response_id)
        .bind(item.item_type)
        .bind(role_str)
        .bind(item.content)
        .bind(item.created_at)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn store_output_item(&self, item: OutputItem) -> Result<()> {
        // Convert role to string if present
        let role_str = item.role.as_ref().map(|r| r.to_string());

        sqlx::query(
            r#"
            INSERT INTO output_items (id, response_id, item_type, role, content, status, created_at) 
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#
        )
        .bind(item.id)
        .bind(item.response_id)
        .bind(item.item_type)
        .bind(role_str)
        .bind(item.content)
        .bind(item.status)
        .bind(item.created_at)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn get_response(&self, response_id: &str) -> Result<Option<ResponseSession>> {
        let row = sqlx::query(
            r#"
            SELECT id, object, created_at, status, model, previous_response_id, 
                   instructions, max_output_tokens, temperature, top_p, store, 
                   metadata, user_id, safety_identifier, prompt_cache_key,
                   usage_input_tokens, usage_output_tokens, usage_total_tokens,
                   error, incomplete_details 
            FROM responses 
            WHERE id = ?1
            "#
        )
        .bind(response_id)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(row) = row {
            // Parse metadata from JSON string if present
            let metadata = row.get::<Option<String>, _>("metadata")
                .as_ref()
                .map(|json_str| serde_json::from_str(json_str))
                .transpose()?;

            Ok(Some(ResponseSession {
                id: row.get("id"),
                object: row.get("object"),
                created_at: row.get("created_at"),
                status: row.get("status"),
                model: row.get("model"),
                previous_response_id: row.get("previous_response_id"),
                instructions: row.get("instructions"),
                max_output_tokens: row.get("max_output_tokens"),
                temperature: row.get("temperature"),
                top_p: row.get("top_p"),
                store: row.get("store"),
                metadata,
                user_id: row.get("user_id"),
                safety_identifier: row.get("safety_identifier"),
                prompt_cache_key: row.get("prompt_cache_key"),
                usage_input_tokens: row.get("usage_input_tokens"),
                usage_output_tokens: row.get("usage_output_tokens"),
                usage_total_tokens: row.get("usage_total_tokens"),
                error: row.get("error"),
                incomplete_details: row.get("incomplete_details"),
            }))
        } else {
            Ok(None)
        }
    }

    pub async fn get_conversation_history(&self, response_id: &str) -> Result<Vec<ResponseSession>> {
        let mut responses = Vec::new();
        let mut current_id = Some(response_id.to_string());

        while let Some(id) = current_id {
            if let Some(response) = self.get_response(&id).await? {
                current_id = response.previous_response_id.clone();
                responses.push(response);
            } else {
                break;
            }
        }

        // Reverse to get chronological order
        responses.reverse();
        Ok(responses)
    }

    pub async fn get_input_items(&self, response_id: &str) -> Result<Vec<InputItem>> {
        let rows = sqlx::query(
            r#"
            SELECT id, response_id, item_type, role, content, created_at 
            FROM input_items 
            WHERE response_id = ?1 
            ORDER BY created_at ASC
            "#
        )
        .bind(response_id)
        .fetch_all(&self.pool)
        .await?;

        let mut items = Vec::new();
        for row in rows {
            // Parse role from string if present
            let role = row.get::<Option<String>, _>("role")
                .as_ref()
                .map(|role_str| role_str.parse())
                .transpose()?;

            items.push(InputItem {
                id: row.get("id"),
                response_id: row.get("response_id"),
                item_type: row.get("item_type"),
                role,
                content: row.get("content"),
                created_at: row.get("created_at"),
            });
        }

        Ok(items)
    }

    pub async fn get_output_items(&self, response_id: &str) -> Result<Vec<OutputItem>> {
        let rows = sqlx::query(
            r#"
            SELECT id, response_id, item_type, role, content, status, created_at 
            FROM output_items 
            WHERE response_id = ?1 
            ORDER BY created_at ASC
            "#
        )
        .bind(response_id)
        .fetch_all(&self.pool)
        .await?;

        let mut items = Vec::new();
        for row in rows {
            // Parse role from string if present
            let role = row.get::<Option<String>, _>("role")
                .as_ref()
                .map(|role_str| role_str.parse())
                .transpose()?;

            items.push(OutputItem {
                id: row.get("id"),
                response_id: row.get("response_id"),
                item_type: row.get("item_type"),
                role,
                content: row.get("content"),
                status: row.get("status"),
                created_at: row.get("created_at"),
            });
        }

        Ok(items)
    }

    pub async fn delete_response(&self, response_id: &str) -> Result<bool> {
        let result = sqlx::query(
            "DELETE FROM responses WHERE id = ?1"
        )
        .bind(response_id)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn update_response_status(&self, response_id: &str, status: &str) -> Result<()> {
        sqlx::query(
            "UPDATE responses SET status = ?1 WHERE id = ?2"
        )
        .bind(status)
        .bind(response_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}