use futures::stream::StreamExt;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::io::Read;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::broadcast;

pub mod tools;
use tools::ToolCall;

pub type DbPool = Pool<SqliteConnectionManager>;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ChatActivityState {
    Idle,
    Thinking,
    AwaitingToolConfirmation,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ChatChange {
    Upsert { id: i64 },
    Deleted { id: i64 },
    Activity { id: i64, state: ChatActivityState },
}

pub fn default_database_url() -> String {
    format!("{}/data/chat.db", env!("CARGO_MANIFEST_DIR"))
}

pub fn database_url() -> String {
    env::var("DATABASE_URL").unwrap_or_else(|_| default_database_url())
}

pub fn model_name() -> String {
    get_env_or("MODEL_NAME", "gemma2:9b")
}

pub fn ollama_url() -> String {
    get_env_or("OLLAMA_URL", "http://localhost:11434")
}

pub fn socket_path() -> PathBuf {
    let db_url = database_url();
    let mut p = PathBuf::from(&db_url);
    p.pop();
    p.push("events.sock");
    p
}

pub fn start_event_server() -> broadcast::Sender<ChatChange> {
    let (tx, _) = broadcast::channel::<ChatChange>(64);
    let tx2 = tx.clone();
    let path = socket_path();

    tokio::spawn(async move {
        let _ = std::fs::remove_file(&path);
        let listener = match tokio::net::UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("event socket bind failed: {e}");
                return;
            }
        };
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => continue,
            };
            let mut rx = tx2.subscribe();
            let tx3 = tx2.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                use tokio::io::AsyncWriteExt;
                let (reader, mut writer) = tokio::io::split(stream);
                let writer_task = tokio::spawn(async move {
                    loop {
                        match rx.recv().await {
                            Ok(change) => {
                                let mut msg =
                                    serde_json::to_string(&change).unwrap_or_default();
                                msg.push('\n');
                                if writer.write_all(msg.as_bytes()).await.is_err() {
                                    break;
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(_) => break,
                        }
                    }
                });

                let mut reader = tokio::io::BufReader::new(reader);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            if let Ok(change) = serde_json::from_str::<ChatChange>(line.trim_end())
                            {
                                let _ = tx3.send(change);
                            }
                        }
                        Err(_) => break,
                    }
                }
                writer_task.abort();
            });
        }
    });

    tx
}

pub fn publish_chat_change(change: &ChatChange) -> Result<(), String> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path)
        .map_err(|e| format!("Failed to connect to event socket {}: {e}", path.display()))?;
    let mut msg = serde_json::to_string(change).map_err(|e| format!("Serialize event: {e}"))?;
    msg.push('\n');
    stream
        .write_all(msg.as_bytes())
        .map_err(|e| format!("Failed to publish event: {e}"))?;
    Ok(())
}

pub fn socket_inode(path: &std::path::Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            m.ino()
        }
        #[cfg(not(unix))]
        {
            0
        }
    })
}

pub struct EventClient {
    stream: UnixStream,
    buf: Vec<u8>,
    pub ino: u64,
}

impl EventClient {
    pub fn connect(path: &std::path::Path) -> Option<Self> {
        let ino = socket_inode(path)?;
        UnixStream::connect(path).ok().map(|s| {
            s.set_nonblocking(true).ok();
            Self {
                stream: s,
                buf: Vec::with_capacity(1024),
                ino,
            }
        })
    }

    pub fn try_recv(&mut self) -> Option<Option<ChatChange>> {
        let mut tmp = [0u8; 1024];
        loop {
            match self.stream.read(&mut tmp) {
                Ok(0) => return None,
                Ok(n) => {
                    self.buf.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = self.buf.drain(..=pos).collect();
                        return Some(serde_json::from_slice(&line[..line.len() - 1]).ok());
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => return Some(None),
                Err(_) => return None,
            }
        }
    }
}

#[derive(Deserialize)]
pub struct ChatRequest {
    pub chat_id: Option<i64>,
    pub message: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct OllamaChunk {
    pub response: String,
    pub done: bool,
}

#[derive(Serialize)]
pub struct OllamaRequest {
    pub model: String,
    pub prompt: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ChatSummary {
    pub id: i64,
    pub title: String,
    pub created_at: String,
    pub message_count: i64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct MessageOut {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub created_at: String,
}

pub fn create_pool(database_url: &str) -> DbPool {
    if let Some(parent) = std::path::Path::new(database_url).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).ok();
    }
    let manager = SqliteConnectionManager::file(database_url);
    Pool::builder()
        .max_size(5)
        .build(manager)
        .expect("Failed to create DB pool")
}

pub fn init_db(pool: &DbPool) {
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

pub fn list_chats(pool: &DbPool) -> Result<Vec<ChatSummary>, String> {
    let conn = pool.get().map_err(|e| format!("Pool: {e}"))?;
    let mut stmt = conn
        .prepare(
            "SELECT c.id, c.title, c.created_at, COUNT(m.id) as message_count
             FROM chats c
             LEFT JOIN messages m ON m.chat_id = c.id
             GROUP BY c.id
             ORDER BY c.created_at DESC, c.id DESC",
        )
        .map_err(|e| format!("{e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ChatSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                created_at: row.get(2)?,
                message_count: row.get(3)?,
            })
        })
        .map_err(|e| format!("{e}"))?;
    let mut chats = Vec::new();
    for row in rows {
        chats.push(row.map_err(|e| format!("{e}"))?);
    }
    Ok(chats)
}

pub fn create_chat(pool: &DbPool) -> Result<ChatSummary, String> {
    let conn = pool.get().map_err(|e| format!("Pool: {e}"))?;
    conn.execute("INSERT INTO chats (title) VALUES ('New Chat')", [])
        .map_err(|e| format!("{e}"))?;
    let id = conn.last_insert_rowid();
    let mut stmt = conn
        .prepare("SELECT id, title, created_at FROM chats WHERE id = ?1")
        .map_err(|e| format!("{e}"))?;
    let chat = stmt
        .query_row(params![id], |row| {
            Ok(ChatSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                created_at: row.get(2)?,
                message_count: 0,
            })
        })
        .map_err(|e| format!("{e}"))?;
    Ok(chat)
}

pub fn delete_chat(pool: &DbPool, id: i64) -> Result<(), String> {
    let conn = pool.get().map_err(|e| format!("Pool: {e}"))?;
    conn.execute("DELETE FROM messages WHERE chat_id = ?1", params![id])
        .map_err(|e| format!("{e}"))?;
    conn.execute("DELETE FROM chats WHERE id = ?1", params![id])
        .map_err(|e| format!("{e}"))?;
    Ok(())
}

pub fn get_messages(pool: &DbPool, chat_id: i64) -> Result<Vec<MessageOut>, String> {
    let conn = pool.get().map_err(|e| format!("Pool: {e}"))?;
    let mut stmt = conn
        .prepare(
            "SELECT id, role, content, created_at
             FROM messages
             WHERE chat_id = ?1
             ORDER BY created_at ASC",
        )
        .map_err(|e| format!("{e}"))?;
    let rows = stmt
        .query_map(params![chat_id], |row| {
            Ok(MessageOut {
                id: row.get(0)?,
                role: row.get(1)?,
                content: row.get(2)?,
                created_at: row.get(3)?,
            })
        })
        .map_err(|e| format!("{e}"))?;
    let mut messages = Vec::new();
    for row in rows {
        messages.push(row.map_err(|e| format!("{e}"))?);
    }
    Ok(messages)
}

pub fn add_message(pool: &DbPool, chat_id: i64, role: &str, content: &str) -> Result<(), String> {
    let conn = pool.get().map_err(|e| format!("Pool: {e}"))?;
    conn.execute(
        "INSERT INTO messages (chat_id, role, content) VALUES (?1, ?2, ?3)",
        params![chat_id, role, content],
    )
    .map_err(|e| format!("{e}"))?;
    Ok(())
}

pub fn update_title_from_message(
    pool: &DbPool,
    chat_id: i64,
    message: &str,
) -> Result<(), String> {
    let conn = pool.get().map_err(|e| format!("Pool: {e}"))?;
    let current_title: String = conn
        .query_row(
            "SELECT title FROM chats WHERE id = ?1",
            params![chat_id],
            |row| row.get(0),
        )
        .unwrap_or_default();
    if current_title == "New Chat" {
        let new_title: String = message.chars().take(50).collect();
        conn.execute(
            "UPDATE chats SET title = ?1 WHERE id = ?2",
            params![new_title, chat_id],
        )
        .map_err(|e| format!("{e}"))?;
    }
    Ok(())
}

fn build_ollama_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .expect("Failed to build Ollama HTTP client")
}

pub fn shared_ollama_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(build_ollama_http_client)
}

pub async fn query_ollama(
    ollama_url: &str,
    model: &str,
    prompt: &str,
) -> Result<String, String> {
    let client = shared_ollama_http_client();
    let request = OllamaRequest {
        model: model.to_string(),
        prompt: prompt.to_string(),
    };

    let response = client
        .post(format!("{}/api/generate", ollama_url))
        .json(&request)
        .send()
        .await
        .map_err(|e| format!("Failed to connect to Ollama: {e}"))?;

    let mut full_response = String::new();
    let mut stream = Box::pin(response.bytes_stream());
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Stream error: {e}"))?;
        let text = String::from_utf8_lossy(&chunk);
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            if let Ok(data) = serde_json::from_str::<OllamaChunk>(line) {
                let cleaned = data
                    .response
                    .replace("<think>", "")
                    .replace("</think>", "");
                full_response.push_str(&cleaned);
            }
        }
    }
    Ok(full_response)
}

pub fn get_env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

// ── Ollama Chat API types (for tool-enabled chat) ──

fn deserialize_string_or_empty<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaChatMessage {
    pub role: String,
    #[serde(deserialize_with = "deserialize_string_or_empty")]
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OllamaChatRequest {
    pub model: String,
    pub messages: Vec<OllamaChatMessage>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<tools::ToolDefinition>>,
}

#[derive(Debug, Deserialize)]
pub struct OllamaChatResponse {
    pub message: OllamaChatMessage,
    pub done: bool,
}

fn extract_tool_calls_from_response(message: &OllamaChatMessage) -> Vec<ToolCall> {
    let native_tcs = message.tool_calls.clone().unwrap_or_default();
    if !native_tcs.is_empty() {
        native_tcs
    } else {
        tools::parse_tool_calls_from_text(&message.content)
    }
}

fn latest_user_message(messages: &[OllamaChatMessage]) -> Option<&str> {
    messages
        .iter()
        .rev()
        .find(|msg| msg.role == "user")
        .map(|msg| msg.content.as_str())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolChatIntent {
    General,
    CreateFile,
    EditFile,
}

fn contains_any(content: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|phrase| content.contains(phrase))
}

fn suggested_write_path_for_request(content: &str) -> &'static str {
    let lower = content.to_lowercase();
    if lower.contains("python") {
        "/tmp/script.py"
    } else if lower.contains("javascript") || lower.contains("node") {
        "/tmp/script.js"
    } else if lower.contains("typescript") {
        "/tmp/script.ts"
    } else if lower.contains("rust") {
        "/tmp/main.rs"
    } else if lower.contains("bash") || lower.contains("shell") || lower.contains("sh script") {
        "/tmp/script.sh"
    } else {
        "/tmp/tool_output.txt"
    }
}

fn classify_tool_chat_intent(content: &str) -> ToolChatIntent {
    let lower = content.to_lowercase();
    let edit_request = contains_any(&lower, &[
        "edit this file",
        "modify this file",
        "update this file",
        "edit the file",
        "modify the file",
        "update the file",
        "change this file",
        "change the file",
        "patch this file",
        "patch the file",
        "rewrite this file",
        "rewrite the file",
    ]);
    if edit_request {
        return ToolChatIntent::EditFile;
    }

    let has_create_verb = contains_any(
        &lower,
        &[
            "create",
            "make",
            "write",
            "save",
            "generate",
            "build",
        ],
    );
    let mentions_file_target = contains_any(
        &lower,
        &[
            " file",
            " script",
            ".py",
            ".js",
            ".ts",
            ".rs",
            ".sh",
            " python script",
            " javascript script",
            " bash script",
            " shell script",
        ],
    );
    let direct_create_request = contains_any(
        &lower,
        &[
            "write code",
            "create code",
            "write me a",
            "make me a",
            "create me a",
        ],
    );
    let create_request = (has_create_verb && mentions_file_target) || direct_create_request;
    if create_request {
        ToolChatIntent::CreateFile
    } else {
        ToolChatIntent::General
    }
}

fn tool_calls_satisfy_intent(intent: ToolChatIntent, tool_calls: &[ToolCall]) -> bool {
    match intent {
        ToolChatIntent::General => true,
        ToolChatIntent::CreateFile => tool_calls
            .iter()
            .any(|tool_call| tool_call.function.name == "write_file"),
        ToolChatIntent::EditFile => tool_calls
            .iter()
            .any(|tool_call| tool_call.function.name == "edit_file"),
    }
}

fn tool_results_satisfy_intent(intent: ToolChatIntent, messages: &[OllamaChatMessage]) -> bool {
    let required_tool = match intent {
        ToolChatIntent::General => return false,
        ToolChatIntent::CreateFile => "write_file",
        ToolChatIntent::EditFile => "edit_file",
    };

    messages.iter().rev().any(|message| {
        message.role == "tool" && message.name.as_deref() == Some(required_tool)
    })
}

fn extract_file_content_from_response_text(text: &str) -> String {
    let code_blocks = tools::extract_code_blocks(text);
    if !code_blocks.is_empty() {
        return code_blocks
            .into_iter()
            .map(|block| block.code)
            .collect::<Vec<_>>()
            .join("\n\n")
            .trim()
            .to_string();
    }

    tools::strip_tool_calls_from_text(text).trim().to_string()
}

async fn generate_create_file_tool_call(
    client: &reqwest::Client,
    url: &str,
    model: &str,
    user_request: &str,
) -> Result<ToolCall, String> {
    let path = suggested_write_path_for_request(user_request).to_string();
    let request = OllamaChatRequest {
        model: model.to_string(),
        messages: vec![
            OllamaChatMessage {
                role: "system".to_string(),
                content: "Return only the full file contents for the requested file. Do not include markdown fences, explanations, XML tags, or tool-call syntax.".to_string(),
                tool_calls: None,
                name: None,
            },
            OllamaChatMessage {
                role: "user".to_string(),
                content: format!(
                    "Create the requested file content for this request:\n{user_request}\n\nTarget path: {path}"
                ),
                tool_calls: None,
                name: None,
            },
        ],
        stream: false,
        tools: None,
    };

    let response = client
        .post(url)
        .json(&request)
        .send()
        .await
        .map_err(|e| format!("Failed to connect to Ollama for file content generation: {e}"))?;

    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| format!("Failed to read file generation response body: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "Ollama returned error {status} while generating file content: {text}"
        ));
    }

    let resp: OllamaChatResponse = serde_json::from_str(&text).map_err(|e| {
        let preview: String = text.chars().take(500).collect();
        format!("Failed to parse file generation response ({e}): {preview}")
    })?;
    let content = extract_file_content_from_response_text(&resp.message.content);
    if content.is_empty() {
        return Err("Model returned empty file content for create-file request.".to_string());
    }

    Ok(ToolCall {
        function: tools::ToolCallFunction {
            name: "write_file".to_string(),
            arguments: serde_json::json!({
                "path": path,
                "content": content,
            }),
        },
    })
}

fn response_claims_file_change_without_tool_call(content: &str) -> bool {
    let lower = content.to_lowercase();
    let mentions_tmp = lower.contains("/tmp/");
    let mentions_code_block = content.contains("```");
    let claims_file_write = [
        "i created",
        "i've created",
        "i saved",
        "i wrote",
        "i made",
        "created the file",
        "saved the file",
        "written to",
        "file has been created",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase));

    (mentions_tmp && claims_file_write) || mentions_code_block
}

fn should_force_tool_retry(
    requested_tools: bool,
    intent: ToolChatIntent,
    intent_already_satisfied: bool,
    tool_calls: &[ToolCall],
    clean_text: &str,
) -> bool {
    if !requested_tools {
        return false;
    }

    if intent_already_satisfied {
        return false;
    }

    if !tool_calls_satisfy_intent(intent, tool_calls) && intent != ToolChatIntent::General
    {
        return true;
    }

    if !tool_calls.is_empty() {
        return false;
    }

    if response_claims_file_change_without_tool_call(clean_text) {
        return true;
    }

    clean_text.contains("```") && intent != ToolChatIntent::General
}

fn correction_message_for_intent(intent: ToolChatIntent, latest_user_message: Option<&str>) -> String {
    match intent {
        ToolChatIntent::CreateFile => {
            let path = latest_user_message
                .map(suggested_write_path_for_request)
                .unwrap_or("/tmp/tool_output.txt");
            format!(
                "Your previous reply should have been a tool call. Retry now.\n\
                Return only a single <tool_call> block in this exact shape:\n\
                <tool_call>\n\
                {{\"function\": {{\"name\": \"write_file\", \"arguments\": {{\"path\": \"{path}\", \"content\": \"...\"}}}}}}\n\
                </tool_call>\n\
                Do not ask the user to continue after the tool call. Do not reply with plain text before the tool call."
            )
        }
        ToolChatIntent::EditFile => "Your previous reply should have been a tool call. Retry now.\n\
                Return only a single <tool_call> block in this exact shape:\n\
                <tool_call>\n\
                {\"function\": {\"name\": \"edit_file\", \"arguments\": {\"path\": \"/absolute/path\", \"old_string\": \"...\", \"new_string\": \"...\"}}}\n\
                </tool_call>\n\
                Do not reply with plain text before the tool call."
            .to_string(),
        ToolChatIntent::General => "Your previous reply should have been a tool call. Retry now and emit the appropriate tool call instead of plain text."
            .to_string(),
    }
}

pub fn get_coding_system_prompt() -> String {
    "You are a coding assistant with filesystem tools.\n\
    Use tools when you need to inspect files, create files, edit files, or run commands.\n\
    If the user asks for a script, source file, or file edit, do not answer with prose first. Emit the tool call immediately.\n\
    When using the text fallback, emit a single <tool_call>...</tool_call> block with valid JSON for the tool call.\n\
    After tool results are returned, continue the task using those results.\n\
    Do not tell the user to continue after a tool call; emit the tool call yourself."
        .to_string()
}

pub fn build_messages_from_db(pool: &DbPool, chat_id: i64) -> Result<Vec<OllamaChatMessage>, String> {
    let db_msgs = get_messages(pool, chat_id)?;
    let mut messages = Vec::new();
    messages.push(OllamaChatMessage {
        role: "system".to_string(),
        content: get_coding_system_prompt(),
        tool_calls: None,
        name: None,
    });
    for m in &db_msgs {
        messages.push(OllamaChatMessage {
            role: m.role.clone(),
            content: m.content.clone(),
            tool_calls: None,
            name: None,
        });
    }
    Ok(messages)
}

pub async fn chat_with_ollama(
    ollama_url: &str,
    model: &str,
    messages: Vec<OllamaChatMessage>,
    tools: Option<Vec<tools::ToolDefinition>>,
) -> Result<OllamaChatResponse, String> {
    let client = shared_ollama_http_client();

    let url = format!("{}/api/chat", ollama_url);
    let requested_tools = tools.clone();
    let mut active_tools = tools;
    let mut working_messages = messages;
    let latest_user_message = latest_user_message(&working_messages).map(str::to_string);
    let intent = latest_user_message.as_deref()
        .map(classify_tool_chat_intent)
        .unwrap_or(ToolChatIntent::General);
    let intent_already_satisfied =
        tool_results_satisfy_intent(intent, &working_messages);
    let mut corrected_tool_retries = 0u8;

    loop {
        let request = OllamaChatRequest {
            model: model.to_string(),
            messages: working_messages.clone(),
            stream: false,
            tools: active_tools.clone(),
        };
        let response = client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| format!("Failed to connect to Ollama: {e}"))?;

        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| format!("Failed to read Ollama response body: {e}"))?;

        let text = String::from_utf8_lossy(&bytes).to_string();

        if !status.is_success() {
            let err = format!("Ollama returned error {status}: {text}");
            if active_tools.is_some() && (err.contains("does not support tools") || err.contains("not supported")) {
                log::warn!("Model does not support tools, retrying without: {err}");
                active_tools = None;
                continue;
            }
            return Err(err);
        }

        let resp: OllamaChatResponse = serde_json::from_str(&text).map_err(|e| {
            let preview: String = text.chars().take(500).collect();
            format!("Failed to parse Ollama response ({e}): {preview}")
        })?;
        let tool_calls = tools::normalize_tool_calls(&extract_tool_calls_from_response(&resp.message));
        let clean_text = tools::strip_tool_calls_from_text(&resp.message.content);

        if requested_tools.is_some()
            && intent == ToolChatIntent::CreateFile
            && !intent_already_satisfied
            && !tool_calls_satisfy_intent(intent, &tool_calls)
        {
            let user_request = latest_user_message
                .as_deref()
                .ok_or("Missing user request for create-file tool synthesis.")?;
            let synthesized_tool_call =
                generate_create_file_tool_call(client, &url, model, user_request).await?;
            return Ok(OllamaChatResponse {
                message: OllamaChatMessage {
                    role: resp.message.role,
                    content: String::new(),
                    tool_calls: Some(vec![tools::normalize_tool_call(&synthesized_tool_call)]),
                    name: None,
                },
                done: resp.done,
            });
        }

        if should_force_tool_retry(
            requested_tools.is_some(),
            intent,
            intent_already_satisfied,
            &tool_calls,
            &clean_text,
        ) {
            if corrected_tool_retries >= 2 {
                return Err(
                    "Model failed to issue the required tool call for the requested file operation."
                        .to_string(),
                );
            }

            corrected_tool_retries += 1;
            working_messages.push(OllamaChatMessage {
                role: "assistant".to_string(),
                content: clean_text,
                tool_calls: None,
                name: None,
            });
            working_messages.push(OllamaChatMessage {
                role: "system".to_string(),
                content: correction_message_for_intent(intent, latest_user_message.as_deref()),
                tool_calls: None,
                name: None,
            });
            active_tools = requested_tools.clone();
            continue;
        }

        return Ok(resp);
    }
}

pub type SessionMap = Arc<Mutex<HashMap<String, SessionState>>>;

pub struct SessionState {
    pub messages: Vec<OllamaChatMessage>,
    pub chat_id: i64,
}

pub fn generate_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    format!("s{:x}", nanos)
}
