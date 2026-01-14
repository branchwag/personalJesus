use actix_cors::Cors;
use actix_files as fs;
use actix_web::{web, App, HttpResponse, HttpServer, Result};
use futures::stream::StreamExt;
use log::info;
use serde::{Deserialize, Serialize};
use std::env;

#[derive(Deserialize)]
struct ChatRequest {
    message: String,
}

#[derive(Deserialize)]
struct OllamaResponse {
    response: String,
    done: bool,
}

#[derive(Serialize)]
struct OllamaRequest {
    model: String,
    prompt: String,
}

async fn chat(req: web::Json<ChatRequest>) -> Result<HttpResponse> {
    let ollama_url = env::var("OLLAMA_URL").unwrap_or_else(|_| "http://ollama:11434".to_string());
    let model = env::var("MODEL_NAME").unwrap_or_else(|_| "mistral:7b".to_string());

    info!("Processing chat request with message: {}", req.message);

    let client = reqwest::Client::new();
    let ollama_request = OllamaRequest {
        model,
        prompt: req.message.clone(),
    };

    let response = client
        .post(format!("{}/api/generate", ollama_url))
        .json(&ollama_request)
        .send()
        .await;

    match response {
        Ok(resp) => {
            let stream = resp.bytes_stream().map(move |item| match item {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let lines: Vec<&str> =
                        text.split('\n').filter(|l| !l.trim().is_empty()).collect();

                    let mut result = String::new();
                    for line in lines {
                        if let Ok(data) = serde_json::from_str::<OllamaResponse>(line) {
                            // Remove <think> tags from the response
                            let cleaned =
                                data.response.replace("<think>", "").replace("</think>", "");
                            result.push_str(&cleaned);
                        }
                    }
                    Ok::<_, actix_web::Error>(web::Bytes::from(result))
                }
                Err(e) => {
                    log::error!("Stream error: {}", e);
                    Ok(web::Bytes::from(format!("Error: {}", e)))
                }
            });

            Ok(HttpResponse::Ok()
                .content_type("text/plain; charset=utf-8")
                .streaming(stream))
        }
        Err(e) => {
            log::error!("Failed to connect to Ollama: {}", e);
            Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to connect to Ollama: {}", e)
            })))
        }
    }
}

async fn health() -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(serde_json::json!({
        "status": "ok"
    })))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    let port = env::var("PORT")
        .unwrap_or_else(|_| "8080".to_string())
        .parse::<u16>()
        .unwrap_or(8080);

    info!("Starting server on http://0.0.0.0:{}", port);

    HttpServer::new(|| {
        let cors = Cors::default()
            .allow_any_origin()
            .allow_any_method()
            .allow_any_header()
            .max_age(3600);

        App::new()
            .wrap(cors)
            .route("/api/chat", web::post().to(chat))
            .route("/health", web::get().to(health))
            .service(fs::Files::new("/", "./static").index_file("index.html"))
    })
    .bind(("0.0.0.0", port))?
    .run()
    .await
}
