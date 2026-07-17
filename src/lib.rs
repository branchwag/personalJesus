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
pub enum ChatChange {
    Upsert { id: i64 },
    Deleted { id: i64 },
}

pub fn socket_path() -> PathBuf {
    let db_url = get_env_or("DATABASE_URL", "data/chat.db");
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
    if let Some(parent) = std::path::Path::new(database_url).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
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
             ORDER BY c.created_at DESC",
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

pub fn get_coding_system_prompt() -> String {
    format!(
        "You are a coding assistant with direct filesystem access via tools. \
        You MUST use your tools to create and edit files — never describe or show code.\n\n\
        AVAILABLE TOOLS:\n\
        - write_file(path, content): Create a new file (IMMEDIATELY creates it)\n\
        - edit_file(path, old_string, new_string): Edit an existing file\n\
        - read_file(path): Read a file\n\
        - run_command(command): Run a shell command\n\
        - glob(pattern): Find files by pattern\n\
        - grep(pattern, path): Search file contents\n\
        - read_directory(path): List directory contents\n\n\
        HOW TO USE TOOLS:\n\
        To call a tool, output a <tool_call> tag in your response like this:\n\
        <tool_call>\n\
        {{\"function\": {{\"name\": \"write_file\", \"arguments\": {{\"path\": \"/tmp/example.py\", \"content\": \"print('hello')\"}}}}}}\n\
        </tool_call>\n\n\
        ABSOLUTE RULES (you MUST follow these):\n\
        1. When asked to create a script, code, or file — call write_file NOW via <tool_call>. Do not describe what you will write.\n\
        2. When asked to modify code — call edit_file NOW via <tool_call>. Do not show the changes, make them.\n\
        3. NEVER output code blocks in your text response. NEVER show the user what the code looks like. Just create the file.\n\
        4. After creating a file, tell the user you created it and where.\n\
        5. If you need the user to know what you created, use run_command with cat/echo after creating the file.\n\
        6. ALWAYS use /tmp/ as the directory for new files. Write to paths like /tmp/filename.ext. \
        NEVER use the user's project directory unless the user explicitly specifies a different path.\n\
        7. Always use absolute paths.\n\n\
        FAILURE MODE: If you output code or descriptions instead of calling tools, you are failing at your job."
    )
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
    let mut corrected_tool_retry = false;

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
        let tool_calls = extract_tool_calls_from_response(&resp.message);
        let clean_text = tools::strip_tool_calls_from_text(&resp.message.content);

        if requested_tools.is_some()
            && tool_calls.is_empty()
            && response_claims_file_change_without_tool_call(&clean_text)
        {
            if corrected_tool_retry {
                return Err(
                    "Model claimed to create or modify a file without issuing a tool call."
                        .to_string(),
                );
            }

            corrected_tool_retry = true;
            working_messages.push(OllamaChatMessage {
                role: "assistant".to_string(),
                content: clean_text,
                tool_calls: None,
                name: None,
            });
            working_messages.push(OllamaChatMessage {
                role: "system".to_string(),
                content: "Your previous reply claimed that a file was created or showed code, but no tool call was emitted and no filesystem change happened. Retry now. If you intend to create or edit a file, respond with a <tool_call> for write_file or edit_file and do not claim success in plain text first.".to_string(),
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
