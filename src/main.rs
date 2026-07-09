use actix_cors::Cors;
use actix_files as fs;
use actix_web::{web, App, HttpResponse, HttpServer, Result};
use futures::stream::StreamExt;
use log::info;
use personal_jesus::tools::{self, ToolCall};
use personal_jesus::*;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

type SessionMap = Arc<Mutex<HashMap<String, SessionState>>>;

fn session_store() -> SessionMap {
    Arc::new(Mutex::new(HashMap::new()))
}

async fn handle_list_chats(pool: web::Data<DbPool>) -> Result<HttpResponse> {
    let pool = pool.get_ref().clone();
    let chats = web::block(move || list_chats(&pool))
        .await
        .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
        .map_err(|e| actix_web::error::ErrorInternalServerError(e))?;
    Ok(HttpResponse::Ok().json(chats))
}

async fn handle_create_chat(pool: web::Data<DbPool>) -> Result<HttpResponse> {
    let pool = pool.get_ref().clone();
    let chat = web::block(move || create_chat(&pool))
        .await
        .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
        .map_err(|e| actix_web::error::ErrorInternalServerError(e))?;
    Ok(HttpResponse::Ok().json(chat))
}

async fn handle_delete_chat(path: web::Path<i64>, pool: web::Data<DbPool>) -> Result<HttpResponse> {
    let id = path.into_inner();
    let pool = pool.get_ref().clone();
    web::block(move || delete_chat(&pool, id))
        .await
        .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
        .map_err(|e| actix_web::error::ErrorInternalServerError(e))?;
    Ok(HttpResponse::Ok().json(serde_json::json!({"ok": true})))
}

async fn handle_get_messages(path: web::Path<i64>, pool: web::Data<DbPool>) -> Result<HttpResponse> {
    let id = path.into_inner();
    let pool = pool.get_ref().clone();
    let messages = web::block(move || get_messages(&pool, id))
        .await
        .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
        .map_err(|e| actix_web::error::ErrorInternalServerError(e))?;
    Ok(HttpResponse::Ok().json(messages))
}

async fn handle_chat(
    req: web::Json<ChatRequest>,
    pool: web::Data<DbPool>,
) -> Result<HttpResponse> {
    let ollama_url = get_env_or("OLLAMA_URL", "http://localhost:11434");
    let model = get_env_or("MODEL_NAME", "gemma2:9b");
    let message = req.message.clone();
    let chat_id = req.chat_id;
    let pool = pool.get_ref().clone();

    info!("Processing chat request for chat_id={:?}, message={}", chat_id, message);

    let chat_id = if let Some(id) = chat_id {
        id
    } else {
        let pool = pool.clone();
        let title: String = message.chars().take(50).collect();
        web::block(move || {
            let conn = pool.get().unwrap();
            conn.execute("INSERT INTO chats (title) VALUES (?1)", params![title])?;
            Ok(conn.last_insert_rowid())
        })
        .await
        .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
        .map_err(|e: rusqlite::Error| actix_web::error::ErrorInternalServerError(e))?
    };

    let pool_clone = pool.clone();
    let msg = message.clone();
    web::block(move || update_title_from_message(&pool_clone, chat_id, &msg))
        .await
        .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
        .map_err(|e| actix_web::error::ErrorInternalServerError(e))?;

    let pool_c = pool.clone();
    let msg_c = message.clone();
    web::block(move || add_message(&pool_c, chat_id, "user", &msg_c))
        .await
        .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
        .map_err(|e| actix_web::error::ErrorInternalServerError(e))?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .unwrap();
    let request = OllamaRequest {
        model,
        prompt: message,
    };

    let response = client
        .post(format!("{}/api/generate", ollama_url))
        .json(&request)
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
                                if let Ok(data) = serde_json::from_str::<OllamaChunk>(line) {
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
                        add_message(&pool2, chat_id2, "assistant", &full_response).ok();
                    })
                    .await
                    .ok();
                }
            });

            Ok(HttpResponse::Ok()
                .insert_header(("X-Chat-Id", chat_id.to_string()))
                .content_type("text/plain; charset=utf-8")
                .streaming(
                    tokio_stream::wrappers::UnboundedReceiverStream::new(rx)
                        .map(|b| Ok::<_, std::io::Error>(b)),
                ))
        }
        Err(e) => {
            log::error!("Failed to connect to Ollama: {}", e);
            Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to connect to Ollama: {e}")
            })))
        }
    }
}

// ── Tool-enabled chat (prompt-based) ──

#[derive(Deserialize)]
pub struct ToolChatRequest {
    pub chat_id: Option<i64>,
    pub message: String,
}

#[derive(Serialize)]
pub struct ToolChatResponse {
    pub r#type: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub chat_id: i64,
}

#[derive(Deserialize)]
pub struct ToolConfirmRequest {
    pub session_id: String,
    pub approved: bool,
    pub modified_paths: Option<Vec<ModifiedPath>>,
}

#[derive(Deserialize, Clone)]
pub struct ModifiedPath {
    pub index: usize,
    pub path: String,
}

async fn handle_tool_chat(
    req: web::Json<ToolChatRequest>,
    pool: web::Data<DbPool>,
    sessions: web::Data<SessionMap>,
) -> Result<HttpResponse> {
    let ollama_url = get_env_or("OLLAMA_URL", "http://localhost:11434");
    let model = get_env_or("MODEL_NAME", "gemma2:9b");
    let message = req.message.clone();
    let pool = pool.get_ref().clone();

    let chat_id = if let Some(id) = req.chat_id {
        id
    } else {
        let pool = pool.clone();
        let title: String = message.chars().take(50).collect();
        web::block(move || {
            let conn = pool.get().unwrap();
            conn.execute("INSERT INTO chats (title) VALUES (?1)", params![title])?;
            Ok(conn.last_insert_rowid())
        })
        .await
        .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
        .map_err(|e: rusqlite::Error| actix_web::error::ErrorInternalServerError(e))?
    };

    let pool_c = pool.clone();
    let msg = message.clone();
    web::block(move || update_title_from_message(&pool_c, chat_id, &msg))
        .await
        .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
        .map_err(|e| actix_web::error::ErrorInternalServerError(e))?;

    let pool_c = pool.clone();
    let msg = message.clone();
    web::block(move || add_message(&pool_c, chat_id, "user", &msg))
        .await
        .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
        .map_err(|e| actix_web::error::ErrorInternalServerError(e))?;

    let pool_db = pool.clone();
    let all_msgs = web::block(move || build_messages_from_db(&pool_db, chat_id))
        .await
        .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
        .map_err(|e| actix_web::error::ErrorInternalServerError(e))?;

    let tools = Some(tools::get_tool_definitions());
    match chat_with_ollama(&ollama_url, &model, all_msgs.clone(), tools).await {
        Ok(resp) => {
            let native_tcs = resp.message.tool_calls.clone().unwrap_or_default();
            let text = &resp.message.content;
            let parsed_tcs = tools::parse_tool_calls_from_text(text);
            let clean_text = tools::strip_tool_calls_from_text(text);

            let tool_calls = if !native_tcs.is_empty() {
                native_tcs
            } else {
                parsed_tcs
            };

            if !clean_text.is_empty() && tool_calls.is_empty() {
                let pool_s = pool.clone();
                let ct = clean_text.clone();
                web::block(move || add_message(&pool_s, chat_id, "assistant", &ct))
                    .await
                    .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
                    .map_err(|e| actix_web::error::ErrorInternalServerError(e))?;
            }

            if tool_calls.is_empty() {
                Ok(HttpResponse::Ok().json(ToolChatResponse {
                    r#type: "text".to_string(),
                    content: clean_text,
                    tool_calls: None,
                    session_id: None,
                    chat_id,
                }))
            } else {
                let mut context = all_msgs;
                context.push(OllamaChatMessage {
                    role: "assistant".to_string(),
                    content: clean_text.clone(),
                    tool_calls: Some(tool_calls.clone()),
                    name: None,
                });

                let session_id = generate_session_id();
                let mut map = sessions.lock().unwrap();
                map.insert(session_id.clone(), SessionState { messages: context, chat_id });

                Ok(HttpResponse::Ok().json(ToolChatResponse {
                    r#type: "tool_calls".to_string(),
                    content: clean_text,
                    tool_calls: Some(tool_calls),
                    session_id: Some(session_id),
                    chat_id,
                }))
            }
        }
        Err(e) => {
            log::error!("Ollama error: {}", e);
            Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Ollama error: {e}")
            })))
        }
    }
}

async fn handle_tool_confirm(
    req: web::Json<ToolConfirmRequest>,
    pool: web::Data<DbPool>,
    sessions: web::Data<SessionMap>,
) -> Result<HttpResponse> {
    let ollama_url = get_env_or("OLLAMA_URL", "http://localhost:11434");
    let model = get_env_or("MODEL_NAME", "gemma2:9b");
    let pool = pool.get_ref().clone();

    let state = {
        let mut map = sessions.lock().unwrap();
        match map.remove(&req.session_id) {
            Some(s) => s,
            None => {
                return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                    "error": "Session expired"
                })));
            }
        }
    };

    // Parse tool calls from the session (from the last assistant message's tool_calls field or text fallback)
    let all_tool_calls: Vec<ToolCall> = state
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant")
        .and_then(|m| {
            m.tool_calls
                .clone()
                .filter(|t| !t.is_empty())
                .or_else(|| {
                    let parsed = tools::parse_tool_calls_from_text(&m.content);
                    if parsed.is_empty() { None } else { Some(parsed) }
                })
        })
        .unwrap_or_default();

    if all_tool_calls.is_empty() {
        return Ok(HttpResponse::BadRequest().json(serde_json::json!({
            "error": "No tool calls found in session"
        })));
    }

    let mut tool_calls = all_tool_calls;
    if let Some(ref paths) = req.modified_paths {
        for mp in paths {
            if mp.index < tool_calls.len() {
                if let Some(args) = tool_calls[mp.index].function.arguments.as_object_mut() {
                    args.insert("path".to_string(), serde_json::Value::String(mp.path.clone()));
                }
            }
        }
    }

    let mut extra = state.messages;

    if req.approved {
        for tc in &tool_calls {
            let result = match tools::execute_tool(tc) {
                Ok(r) => r,
                Err(e) => format!("Error: {e}"),
            };
            extra.push(OllamaChatMessage {
                role: "tool".to_string(),
                content: result,
                tool_calls: None,
                name: Some(tc.function.name.clone()),
            });
        }
    } else {
        extra.push(OllamaChatMessage {
            role: "tool".to_string(),
            content: "The user declined to execute this tool call.".to_string(),
            tool_calls: None,
            name: Some(tool_calls[0].function.name.clone()),
        });
    }

    let tools = Some(tools::get_tool_definitions());
    match chat_with_ollama(&ollama_url, &model, extra.clone(), tools).await {
        Ok(resp) => {
            let native_tcs = resp.message.tool_calls.clone().unwrap_or_default();
            let text = &resp.message.content;
            let parsed_tcs = tools::parse_tool_calls_from_text(text);
            let clean_text = tools::strip_tool_calls_from_text(text);

            let tool_calls = if !native_tcs.is_empty() {
                native_tcs
            } else {
                parsed_tcs
            };

            if !clean_text.is_empty() {
                let pool_s = pool.clone();
                let ct = clean_text.clone();
                web::block(move || add_message(&pool_s, state.chat_id, "assistant", &ct))
                    .await
                    .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
                    .map_err(|e| actix_web::error::ErrorInternalServerError(e))?;
            }

            if tool_calls.is_empty() {
                Ok(HttpResponse::Ok().json(ToolChatResponse {
                    r#type: "text".to_string(),
                    content: clean_text,
                    tool_calls: None,
                    session_id: None,
                    chat_id: state.chat_id,
                }))
            } else {
                let mut context = extra;
                context.push(OllamaChatMessage {
                    role: "assistant".to_string(),
                    content: clean_text.clone(),
                    tool_calls: Some(tool_calls.clone()),
                    name: None,
                });

                let session_id = generate_session_id();
                let mut map = sessions.lock().unwrap();
                map.insert(session_id.clone(), SessionState { messages: context, chat_id: state.chat_id });

                Ok(HttpResponse::Ok().json(ToolChatResponse {
                    r#type: "tool_calls".to_string(),
                    content: clean_text,
                    tool_calls: Some(tool_calls),
                    session_id: Some(session_id),
                    chat_id: state.chat_id,
                }))
            }
        }
        Err(e) => {
            log::error!("Ollama error: {}", e);
            Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Ollama error: {e}")
            })))
        }
    }
}

#[derive(Deserialize)]
struct WriteFileRequest {
    path: String,
    content: String,
}

async fn handle_write_file(req: web::Json<WriteFileRequest>) -> Result<HttpResponse> {
    let path = &req.path;
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            actix_web::error::ErrorInternalServerError(format!("mkdir error: {e}"))
        })?;
    }
    std::fs::write(path, &req.content).map_err(|e| {
        actix_web::error::ErrorInternalServerError(format!("write error: {e}"))
    })?;
    Ok(HttpResponse::Ok().json(serde_json::json!({"ok": true, "path": path, "bytes": req.content.len()})))
}

async fn health() -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(serde_json::json!({"status": "ok"})))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    let port = get_env_or("PORT", "8080").parse::<u16>().unwrap_or(8080);
    let database_url = get_env_or("DATABASE_URL", "data/chat.db");

    let pool = create_pool(&database_url);
    init_db(&pool);

    info!("Starting server on http://0.0.0.0:{}", port);

    let pool_data = web::Data::new(pool);
    let sessions = web::Data::new(session_store());

    HttpServer::new(move || {
        let cors = Cors::default()
            .allow_any_origin()
            .allow_any_method()
            .allow_any_header()
            .max_age(3600);

        App::new()
            .app_data(pool_data.clone())
            .app_data(sessions.clone())
            .wrap(cors)
            .route("/api/chat", web::post().to(handle_chat))
            .route("/api/chat/tools", web::post().to(handle_tool_chat))
            .route("/api/chat/tools/confirm", web::post().to(handle_tool_confirm))
            .route("/api/chats", web::post().to(handle_create_chat))
            .route("/api/chats", web::get().to(handle_list_chats))
            .route("/api/chats/{id}", web::delete().to(handle_delete_chat))
            .route("/api/chats/{id}/messages", web::get().to(handle_get_messages))
            .route("/api/write-file", web::post().to(handle_write_file))
            .route("/health", web::get().to(health))
            .service(fs::Files::new("/", "./static").index_file("index.html"))
    })
    .bind(("0.0.0.0", port))?
    .run()
    .await
}
