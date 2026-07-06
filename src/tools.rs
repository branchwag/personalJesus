use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub function: ToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: serde_json::Value,
}

pub fn get_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "read_file".to_string(),
                description: "Read the contents of a file from the filesystem".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute path to the file" }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "write_file".to_string(),
                description: "Write content to a file (overwrites existing content, creates parent directories)".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute path to the file" },
                        "content": { "type": "string", "description": "Content to write" }
                    },
                    "required": ["path", "content"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "edit_file".to_string(),
                description: "Edit a file by replacing exact text matches. Use for targeted edits.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute path to the file" },
                        "old_string": { "type": "string", "description": "Text to search for (exact match)" },
                        "new_string": { "type": "string", "description": "Text to replace with" }
                    },
                    "required": ["path", "old_string", "new_string"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "run_command".to_string(),
                description: "Run a shell command via `sh -c`. Output is captured and returned.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Shell command to execute" }
                    },
                    "required": ["command"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "glob".to_string(),
                description: "Find files and directories matching a glob pattern".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Glob pattern (e.g. '**/*.rs')" }
                    },
                    "required": ["pattern"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "grep".to_string(),
                description: "Search file contents using a regex pattern. Uses ripgrep if available, falls back to grep.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Regex pattern to search" },
                        "path": { "type": "string", "description": "Directory or file to search (default: current directory)" }
                    },
                    "required": ["pattern"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "read_directory".to_string(),
                description: "List the contents of a directory".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute path to the directory" }
                    },
                    "required": ["path"]
                }),
            },
        },
    ]
}

pub fn execute_tool(tool_call: &ToolCall) -> Result<String, String> {
    let name = &tool_call.function.name;
    let args = &tool_call.function.arguments;

    match name.as_str() {
        "read_file" => {
            let path = args.get("path").and_then(|v| v.as_str()).ok_or("missing path")?;
            std::fs::read_to_string(path).map_err(|e| format!("read_file error: {e}"))
        }
        "write_file" => {
            let path = args.get("path").and_then(|v| v.as_str()).ok_or("missing path")?;
            let content = args.get("content").and_then(|v| v.as_str()).ok_or("missing content")?;
            if let Some(parent) = std::path::Path::new(path).parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("write_file mkdir error: {e}"))?;
            }
            std::fs::write(path, content).map_err(|e| format!("write_file error: {e}"))?;
            Ok(format!("ok wrote {} bytes", content.len()))
        }
        "edit_file" => {
            let path = args.get("path").and_then(|v| v.as_str()).ok_or("missing path")?;
            let old = args.get("old_string").and_then(|v| v.as_str()).ok_or("missing old_string")?;
            let new = args.get("new_string").and_then(|v| v.as_str()).ok_or("missing new_string")?;
            let content = std::fs::read_to_string(path).map_err(|e| format!("edit_file read error: {e}"))?;
            if !content.contains(old) {
                return Err(format!("no match found in {path}"));
            }
            let new_content = content.replace(old, new);
            std::fs::write(path, new_content).map_err(|e| format!("edit_file write error: {e}"))?;
            Ok("ok edited successfully".to_string())
        }
        "run_command" => {
            let cmd = args.get("command").and_then(|v| v.as_str()).ok_or("missing command")?;
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .output()
                .map_err(|e| format!("run_command error: {e}"))?;
            let mut result = String::new();
            if !output.stdout.is_empty() {
                result.push_str(&String::from_utf8_lossy(&output.stdout));
            }
            if !output.stderr.is_empty() {
                if !result.is_empty() { result.push('\n'); }
                result.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            if result.is_empty() {
                result = format!("exit code: {:?}", output.status.code());
            }
            Ok(result)
        }
        "glob" => {
            let pattern = args.get("pattern").and_then(|v| v.as_str()).ok_or("missing pattern")?;
            let entries = glob::glob(pattern).map_err(|e| format!("glob error: {e}"))?;
            let mut paths: Vec<String> = entries.filter_map(|e| e.ok()).map(|p| p.display().to_string()).collect();
            paths.sort();
            if paths.is_empty() { Ok("no matches".to_string()) }
            else { Ok(paths.join("\n")) }
        }
        "grep" => {
            let pattern = args.get("pattern").and_then(|v| v.as_str()).ok_or("missing pattern")?;
            let search_path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let result = std::process::Command::new("rg")
                .args(["-n", pattern, search_path])
                .output();
            let output = match result {
                Ok(o) if o.status.success() || o.status.code() == Some(1) => o,
                _ => {
                    std::process::Command::new("grep")
                        .args(["-rn", pattern, search_path])
                        .output()
                        .map_err(|e| format!("grep error: {e}"))?
                }
            };
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            if stdout.is_empty() { Ok("no matches".to_string()) }
            else { Ok(stdout.trim().to_string()) }
        }
        "read_directory" => {
            let dir = args.get("path").and_then(|v| v.as_str()).ok_or("missing path")?;
            let entries = std::fs::read_dir(dir).map_err(|e| format!("read_dir error: {e}"))?;
            let mut items: Vec<String> = entries
                .filter_map(|e| e.ok())
                .map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        format!("{name}/")
                    } else {
                        name
                    }
                })
                .collect();
            items.sort();
            if items.is_empty() { Ok("empty directory".to_string()) }
            else { Ok(items.join("\n")) }
        }
        _ => Err(format!("unknown tool: {name}")),
    }
}

pub fn parse_tool_calls_from_text(text: &str) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    let mut pos = 0;
    let bytes = text.as_bytes();
    while pos < bytes.len() {
        let remaining = &text[pos..];
        if let Some(start) = remaining.find("<tool_call>") {
            let content_start = pos + start + "<tool_call>".len();
            if let Some(end) = text[content_start..].find("</tool_call>") {
                let json_str = &text[content_start..content_start + end];
                if let Ok(tc) = serde_json::from_str::<ToolCall>(json_str.trim()) {
                    calls.push(tc);
                }
                pos = content_start + end + "</tool_call>".len();
            } else {
                break;
            }
        } else {
            break;
        }
    }
    calls
}

pub fn strip_tool_calls_from_text(text: &str) -> String {
    let mut result = String::new();
    let mut pos = 0;
    loop {
        let remaining = &text[pos..];
        if let Some(start) = remaining.find("<tool_call>") {
            result.push_str(&text[pos..pos + start]);
            if let Some(end) = remaining[start..].find("</tool_call>") {
                pos = pos + start + end + "</tool_call>".len();
            } else {
                result.push_str(remaining);
                break;
            }
        } else {
            result.push_str(remaining);
            break;
        }
    }
    result.trim().to_string()
}

#[derive(Debug, Clone)]
pub struct CodeBlock {
    pub language: String,
    pub code: String,
}

pub fn extract_code_blocks(text: &str) -> Vec<CodeBlock> {
    let mut blocks = Vec::new();
    let mut pos = 0;
    let bytes = text.as_bytes();
    while pos < bytes.len() {
        let remaining = &text[pos..];
        if let Some(start) = remaining.find("```") {
            let content_start = pos + start + 3;
            let rest = &text[content_start..];
            let line_end = rest.find('\n').unwrap_or(rest.len());
            let language = rest[..line_end].trim().to_string();
            let code_start = content_start + line_end + 1;
            if let Some(end) = text[code_start..].find("```") {
                let code = &text[code_start..code_start + end];
                blocks.push(CodeBlock { language, code: code.to_string() });
                pos = code_start + end + 3;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    blocks
}

pub fn tool_call_description(tool_call: &ToolCall) -> String {
    let name = &tool_call.function.name;
    let args = &tool_call.function.arguments;

    let g = |key: &str| -> String {
        args.get(key).and_then(|v| v.as_str()).unwrap_or("?").to_string()
    };

    match name.as_str() {
        "read_file" => format!("Read file: {}", g("path")),
        "write_file" => {
            let content = g("content");
            let preview: String = content.chars().take(80).collect();
            let ellipsis = if content.len() > 80 { "..." } else { "" };
            format!("Write file: {} ({} bytes)\n  {}{}", g("path"), content.len(), preview, ellipsis)
        }
        "edit_file" => {
            format!("Edit file: {}\n  Replace: {}\n  With:    {}", g("path"), g("old_string"), g("new_string"))
        }
        "run_command" => format!("Run: {}", g("command")),
        "glob" => format!("Glob: {}", g("pattern")),
        "grep" => {
            let p = g("path");
            let sp = if p == "?" { ".".to_string() } else { p };
            format!("Grep: '{}' in {}", g("pattern"), sp)
        }
        "read_directory" => format!("List dir: {}", g("path")),
        _ => format!("Tool: {name}({args})"),
    }
}
