use personal_jesus::*;
use std::io::{self, BufRead, Write};

fn read_line(prompt: &str) -> String {
    print!("{prompt}");
    io::stdout().flush().ok();
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line).ok();
    line.trim().to_string()
}

fn show_chats(pool: &DbPool) -> Vec<ChatSummary> {
    let chats = list_chats(pool).unwrap_or_default();
    println!("\n━━━ Chats ━━━");
    if chats.is_empty() {
        println!("  (no chats yet)");
    } else {
        for (i, chat) in chats.iter().enumerate() {
            println!(
                "  {}. {} ({} messages)",
                i + 1,
                chat.title,
                chat.message_count
            );
        }
    }
    println!("  n. New chat");
    println!("  q. Quit");
    chats
}

fn show_messages(messages: &[MessageOut]) {
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    for msg in messages {
        let label = match msg.role.as_str() {
            "user" => "You",
            "assistant" => "AI",
            _ => &msg.role,
        };
        println!("[{label}] {}", msg.content);
        println!();
    }
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
}

async fn interact_with_chat(pool: &DbPool, chat_id: i64) {
    let ollama_url = get_env_or("OLLAMA_URL", "http://localhost:11434");
    let model = get_env_or("MODEL_NAME", "qwen2.5:7b");

    let chats = list_chats(pool).unwrap_or_default();
    let title = chats
        .iter()
        .find(|c| c.id == chat_id)
        .map(|c| c.title.as_str())
        .unwrap_or("Chat");

    println!("\n━━━ {title} ━━━");

    if let Ok(messages) = get_messages(pool, chat_id) {
        show_messages(&messages);
    }

    loop {
        let input = read_line("\nYou: ");
        match input.as_str() {
            "q" | "quit" => {
                println!("Goodbye!");
                std::process::exit(0);
            }
            "b" | "back" => return,
            "" => continue,
            msg => {
                if let Err(e) = update_title_from_message(pool, chat_id, msg) {
                    eprintln!("DB error: {e}");
                }
                if let Err(e) = add_message(pool, chat_id, "user", msg) {
                    eprintln!("DB error: {e}");
                }

                print!("AI: ");
                io::stdout().flush().ok();

                match query_ollama(&ollama_url, &model, msg).await {
                    Ok(response) => {
                        println!("{response}");
                        if let Err(e) = add_message(pool, chat_id, "assistant", &response) {
                            eprintln!("DB error: {e}");
                        }
                    }
                    Err(e) => {
                        eprintln!("\nError: {e}");
                    }
                }
            }
        }
    }
}

async fn run_interactive(pool: &DbPool) {
    println!("Personal Jesus CLI — type q to quit");

    loop {
        let chats = show_chats(pool);
        let input = read_line("\nSelect: ");

        match input.as_str() {
            "q" | "quit" => {
                println!("Goodbye!");
                break;
            }
            "n" => match create_chat(pool) {
                Ok(chat) => {
                    interact_with_chat(pool, chat.id).await;
                }
                Err(e) => eprintln!("Error creating chat: {e}"),
            },
            s => {
                if let Ok(n) = s.parse::<usize>() {
                    if n > 0 && n <= chats.len() {
                        interact_with_chat(pool, chats[n - 1].id).await;
                    } else {
                        eprintln!("Invalid selection");
                    }
                } else {
                    eprintln!("Invalid input");
                }
            }
        }
    }
}

async fn run_one_shot(pool: &DbPool, question: &str) {
    let ollama_url = get_env_or("OLLAMA_URL", "http://localhost:11434");
    let model = get_env_or("MODEL_NAME", "qwen2.5:7b");

    let chat = match create_chat(pool) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {e}");
            return;
        }
    };

    update_title_from_message(pool, chat.id, question).ok();
    add_message(pool, chat.id, "user", question).ok();

    println!("You: {question}");

    print!("AI: ");
    io::stdout().flush().ok();

    match query_ollama(&ollama_url, &model, question).await {
        Ok(response) => {
            println!("{response}");
            add_message(pool, chat.id, "assistant", &response).ok();
        }
        Err(e) => {
            eprintln!("\nError: {e}");
        }
    }
}

#[tokio::main]
async fn main() {
    let database_url = get_env_or("DATABASE_URL", "data/chat.db");
    let pool = create_pool(&database_url);
    init_db(&pool);

    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let question = args[1..].join(" ");
        run_one_shot(&pool, &question).await;
    } else {
        run_interactive(&pool).await;
    }
}
