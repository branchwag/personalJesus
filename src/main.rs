use actix_cors::Cors;
use actix_files as fs;
use actix_web::{web, App, HttpResponse, HttpServer, Result};
use futures::stream::StreamExt;
use log::info;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::env;
use tokio::sync::mpsc;

type DbPool = Pool<SqliteConnectionManager>;

#[derive(Deserialize)]
struct ChatRequest {
    chat_id: Option<i64>,
    message: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct OllamaChunk {
    response: String,
    done: bool,
}

#[derive(Serialize)]
struct OllamaRequest {
    model: String,
    prompt: String,
}

#[derive(Serialize)]
struct ChatSummary {
    id: i64,
    title: String,
    created_at: String,
    message_count: i64,
}

#[derive(Serialize)]
struct MessageOut {
    id: i64,
    role: String,
    content: String,
    created_at: String,
}

fn init_db(pool: &DbPool) {
    let conn = pool.get().unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS chats (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL DEFAULT 'New Chat',
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            chat_id INTEGER NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            FOREIGN KEY (chat_id) REFERENCES chats(id) ON DELETE CASCADE
        );",
    )
    .unwrap();
}

async fn list_chats(pool: web::Data<DbPool>) -> Result<HttpResponse> {
    let pool = pool.get_ref().clone();
    let chats = web::block(move || {
        let conn = pool.get().unwrap();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.title, c.created_at, COUNT(m.id) as message_count
             FROM chats c
             LEFT JOIN messages m ON m.chat_id = c.id
             GROUP BY c.id
             ORDER BY c.created_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ChatSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                created_at: row.get(2)?,
                message_count: row.get(3)?,
            })
        })?;
        let mut chats = Vec::new();
        for row in rows {
            chats.push(row?);
        }
        Ok(chats)
    })
    .await
    .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
    .map_err(|e: rusqlite::Error| actix_web::error::ErrorInternalServerError(e))?;

    Ok(HttpResponse::Ok().json(chats))
}

async fn create_chat(pool: web::Data<DbPool>) -> Result<HttpResponse> {
    let pool = pool.get_ref().clone();
    let chat = web::block(move || {
        let conn = pool.get().unwrap();
        conn.execute("INSERT INTO chats (title) VALUES ('New Chat')", [])?;
        let id = conn.last_insert_rowid();
        let mut stmt = conn.prepare("SELECT id, title, created_at FROM chats WHERE id = ?1")?;
        let chat = stmt.query_row(params![id], |row| {
            Ok(ChatSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                created_at: row.get(2)?,
                message_count: 0,
            })
        })?;
        Ok(chat)
    })
    .await
    .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
    .map_err(|e: rusqlite::Error| actix_web::error::ErrorInternalServerError(e))?;

    Ok(HttpResponse::Ok().json(chat))
}

async fn delete_chat(path: web::Path<i64>, pool: web::Data<DbPool>) -> Result<HttpResponse> {
    let chat_id = path.into_inner();
    let pool = pool.get_ref().clone();
    web::block(move || {
        let conn = pool.get().unwrap();
        conn.execute("DELETE FROM messages WHERE chat_id = ?1", params![chat_id])?;
        conn.execute("DELETE FROM chats WHERE id = ?1", params![chat_id])?;
        Ok::<_, rusqlite::Error>(())
    })
    .await
    .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
    .map_err(|e: rusqlite::Error| actix_web::error::ErrorInternalServerError(e))?;

    Ok(HttpResponse::Ok().json(serde_json::json!({"ok": true})))
}

async fn get_messages(path: web::Path<i64>, pool: web::Data<DbPool>) -> Result<HttpResponse> {
    let chat_id = path.into_inner();
    let pool = pool.get_ref().clone();
    let messages = web::block(move || {
        let conn = pool.get().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, role, content, created_at
             FROM messages
             WHERE chat_id = ?1
             ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![chat_id], |row| {
            Ok(MessageOut {
                id: row.get(0)?,
                role: row.get(1)?,
                content: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        let mut messages = Vec::new();
        for row in rows {
            messages.push(row?);
        }
        Ok(messages)
    })
    .await
    .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
    .map_err(|e: rusqlite::Error| actix_web::error::ErrorInternalServerError(e))?;

    Ok(HttpResponse::Ok().json(messages))
}

async fn chat(
    req: web::Json<ChatRequest>,
    pool: web::Data<DbPool>,
) -> Result<HttpResponse> {
    let ollama_url = env::var("OLLAMA_URL").unwrap_or_else(|_| "http://ollama:11434".to_string());
    let model = env::var("MODEL_NAME").unwrap_or_else(|_| "mistral:7b".to_string());
    let message = req.message.clone();
    let chat_id = req.chat_id;
    let pool = pool.get_ref().clone();

    info!(
        "Processing chat request for chat_id={:?}, message={}",
        chat_id, message
    );

    let chat_id = if let Some(id) = chat_id {
        id
    } else {
        let pool = pool.clone();
        let title: String = message.chars().take(50).collect();
        web::block(move || {
            let conn = pool.get().unwrap();
            conn.execute(
                "INSERT INTO chats (title) VALUES (?1)",
                params![title],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await
        .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
        .map_err(|e: rusqlite::Error| actix_web::error::ErrorInternalServerError(e))?
    };

    let pool_clone = pool.clone();
    let message_clone = message.clone();
    web::block(move || {
        let conn = pool_clone.get().unwrap();
        let current_title: String = conn
            .query_row(
                "SELECT title FROM chats WHERE id = ?1",
                params![chat_id],
                |row| row.get(0),
            )
            .unwrap_or_default();
        if current_title == "New Chat" {
            let new_title: String = message_clone.chars().take(50).collect();
            conn.execute(
                "UPDATE chats SET title = ?1 WHERE id = ?2",
                params![new_title, chat_id],
            )?;
        }
        conn.execute(
            "INSERT INTO messages (chat_id, role, content) VALUES (?1, 'user', ?2)",
            params![chat_id, message_clone],
        )?;
        Ok::<_, rusqlite::Error>(())
    })
    .await
    .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
    .map_err(|e: rusqlite::Error| actix_web::error::ErrorInternalServerError(e))?;

    let client = reqwest::Client::new();
    let ollama_request = OllamaRequest {
        model,
        prompt: message,
    };

    let response = client
        .post(format!("{}/api/generate", ollama_url))
        .json(&ollama_request)
        .send()
        .await;

    match response {
        Ok(resp) => {
            let (tx, rx) = mpsc::unbounded_channel::<web::Bytes>();
            let pool2 = pool.clone();
            let chat_id2 = chat_id;

            tokio::spawn(async move {
                let mut stream = Box::pin(resp.bytes_stream());
                let mut full_response = String::new();

                while let Some(item) = stream.next().await {
                    match item {
                        Ok(bytes) => {
                            let text = String::from_utf8_lossy(&bytes);
                            let lines: Vec<&str> = text
                                .split('\n')
                                .filter(|l| !l.trim().is_empty())
                                .collect();

                            let mut result = String::new();
                            for line in &lines {
                                if let Ok(data) =
                                    serde_json::from_str::<OllamaChunk>(line)
                                {
                                    let cleaned = data
                                        .response
                                        .replace("<think>", "")
                                        .replace("</think>", "");
                                    result.push_str(&cleaned);
                                }
                            }
                            full_response.push_str(&result);
                            let _ = tx.send(web::Bytes::from(result));
                        }
                        Err(e) => {
                            log::error!("Stream error: {}", e);
                            let _ = tx.send(web::Bytes::from(format!("Error: {e}")));
                        }
                    }
                }

                if !full_response.is_empty() {
                    tokio::task::spawn_blocking(move || {
                        if let Ok(conn) = pool2.get() {
                            let _ = conn.execute(
                                "INSERT INTO messages (chat_id, role, content) VALUES (?1, 'assistant', ?2)",
                                params![chat_id2, full_response],
                            );
                        }
                    })
                    .await
                    .ok();
                }
            });

            Ok(HttpResponse::Ok()
                .insert_header(("X-Chat-Id", chat_id.to_string()))
                .content_type("text/plain; charset=utf-8")
                .streaming(tokio_stream::wrappers::UnboundedReceiverStream::new(rx).map(|b| Ok::<_, std::io::Error>(b))))
        }
        Err(e) => {
            log::error!("Failed to connect to Ollama: {}", e);
            Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to connect to Ollama: {e}")
            })))
        }
    }
}

async fn health() -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(serde_json::json!({"status": "ok"})))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    let port = env::var("PORT")
        .unwrap_or_else(|_| "8080".to_string())
        .parse::<u16>()
        .unwrap_or(8080);

    let database_url =
        env::var("DATABASE_URL").unwrap_or_else(|_| "data/chat.db".to_string());

    if let Some(parent) = std::path::Path::new(&database_url).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }

    let manager = SqliteConnectionManager::file(&database_url);
    let pool = Pool::builder()
        .max_size(5)
        .build(manager)
        .expect("Failed to create DB pool");

    init_db(&pool);

    info!("Starting server on http://0.0.0.0:{}", port);

    let pool_data = web::Data::new(pool);

    HttpServer::new(move || {
        let cors = Cors::default()
            .allow_any_origin()
            .allow_any_method()
            .allow_any_header()
            .max_age(3600);

        App::new()
            .app_data(pool_data.clone())
            .wrap(cors)
            .route("/api/chat", web::post().to(chat))
            .route("/api/chats", web::post().to(create_chat))
            .route("/api/chats", web::get().to(list_chats))
            .route("/api/chats/{id}", web::delete().to(delete_chat))
            .route("/api/chats/{id}/messages", web::get().to(get_messages))
            .route("/health", web::get().to(health))
            .service(fs::Files::new("/", "./static").index_file("index.html"))
    })
    .bind(("0.0.0.0", port))?
    .run()
    .await
}
