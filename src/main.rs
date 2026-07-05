use actix_cors::Cors;
use actix_files as fs;
use actix_web::{web, App, HttpResponse, HttpServer, Result};
use futures::stream::StreamExt;
use log::info;
use personal_jesus::{
    add_message, create_chat, create_pool, delete_chat, get_env_or, get_messages, init_db,
    list_chats, update_title_from_message, ChatRequest, DbPool, OllamaChunk, OllamaRequest,
};
use rusqlite::params;
use tokio::sync::mpsc;

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
    let ollama_url = get_env_or("OLLAMA_URL", "http://ollama:11434");
    let model = get_env_or("MODEL_NAME", "qwen2.5:7b");
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
    web::block(move || update_title_from_message(&pool_clone, chat_id, &message_clone))
        .await
        .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
        .map_err(|e| actix_web::error::ErrorInternalServerError(e))?;

    let pool_clone = pool.clone();
    let message_clone = message.clone();
    web::block(move || add_message(&pool_clone, chat_id, "user", &message_clone))
        .await
        .map_err(|e| actix_web::error::ErrorInternalServerError(format!("DB: {e}")))?
        .map_err(|e| actix_web::error::ErrorInternalServerError(e))?;

    let client = reqwest::Client::new();
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

async fn health() -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(serde_json::json!({"status": "ok"})))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    let port = get_env_or("PORT", "8080")
        .parse::<u16>()
        .unwrap_or(8080);
    let database_url = get_env_or("DATABASE_URL", "data/chat.db");

    let pool = create_pool(&database_url);
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
            .route("/api/chat", web::post().to(handle_chat))
            .route("/api/chats", web::post().to(handle_create_chat))
            .route("/api/chats", web::get().to(handle_list_chats))
            .route("/api/chats/{id}", web::delete().to(handle_delete_chat))
            .route("/api/chats/{id}/messages", web::get().to(handle_get_messages))
            .route("/health", web::get().to(health))
            .service(fs::Files::new("/", "./static").index_file("index.html"))
    })
    .bind(("0.0.0.0", port))?
    .run()
    .await
}
