use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use pj::tools::{self, ToolCall};
use pj::*;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::io::{self, stdout, Write};
use tokio::sync::mpsc;

fn strip_code_blocks(text: &str) -> String {
    let mut result = String::new();
    let mut pos = 0;
    while pos < text.len() {
        let remaining = &text[pos..];
        if let Some(start) = remaining.find("```") {
            result.push_str(&text[pos..pos + start]);
            let content_start = pos + start + 3;
            let rest = &text[content_start..];
            let line_end = rest.find('\n').unwrap_or(rest.len());
            let code_start = content_start + line_end + 1;
            if let Some(end) = text[code_start..].find("```") {
                result.push_str(&text[code_start..code_start + end]);
                pos = code_start + end + 3;
            } else {
                result.push_str(&text[pos..]);
                break;
            }
        } else {
            result.push_str(&text[pos..]);
            break;
        }
    }
    result
}

#[derive(PartialEq)]
enum Focus {
    Sidebar,
    Input,
}

enum CliEvent {
    TextReady,
    ToolCalls(Vec<ToolCall>, Vec<OllamaChatMessage>),
    Error(String),
}

struct App {
    chats: Vec<ChatSummary>,
    messages: Vec<MessageOut>,
    active_chat_id: Option<i64>,
    input: String,
    loading: bool,
    sidebar_index: usize,
    focus: Focus,
    scroll: u16,
    exit: bool,
    pool: DbPool,
    ollama_url: String,
    model: String,
    tx: mpsc::UnboundedSender<CliEvent>,
    rx: mpsc::UnboundedReceiver<CliEvent>,
    pending_tool_calls: Option<(Vec<ToolCall>, Vec<OllamaChatMessage>)>,
    waiting_confirmation: bool,
    event_client: Option<EventClient>,
}

impl App {
    fn new(pool: DbPool) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let ollama_url = get_env_or("OLLAMA_URL", "http://localhost:11434");
        let model = get_env_or("MODEL_NAME", "gemma2:9b");
        let mut app = Self {
            chats: vec![],
            messages: vec![],
            active_chat_id: None,
            input: String::new(),
            loading: false,
            sidebar_index: 0,
            focus: Focus::Input,
            scroll: 0,
            exit: false,
            pool,
            ollama_url,
            model,
            tx,
            rx,
            pending_tool_calls: None,
            waiting_confirmation: false,
            event_client: EventClient::connect(&socket_path()),
        };
        app.load_chats();
        app
    }

    fn load_chats(&mut self) {
        self.chats = list_chats(&self.pool).unwrap_or_default();
        if !self.chats.is_empty() && self.sidebar_index >= self.chats.len() {
            self.sidebar_index = self.chats.len() - 1;
        }
    }

    fn load_messages(&mut self) {
        if let Some(id) = self.active_chat_id {
            self.messages = get_messages(&self.pool, id).unwrap_or_default();
        } else {
            self.messages = vec![];
        }
        self.scroll = 0;
    }

    fn select_chat(&mut self, index: usize) {
        if index < self.chats.len() {
            self.active_chat_id = Some(self.chats[index].id);
            self.load_messages();
            self.focus = Focus::Input;
        }
    }

    fn new_chat(&mut self) {
        if let Ok(chat) = create_chat(&self.pool) {
            self.load_chats();
            self.sidebar_index = 0;
            self.active_chat_id = Some(chat.id);
            self.messages = vec![];
            self.input.clear();
            self.focus = Focus::Input;
            self.scroll = 0;
        }
    }

    fn send_message(&mut self) {
        let message = self.input.trim().to_string();
        if message.is_empty() || self.loading {
            return;
        }
        self.input.clear();

        let chat_id = match self.active_chat_id {
            Some(id) => id,
            None => {
                self.new_chat();
                match self.active_chat_id {
                    Some(id) => id,
                    None => return,
                }
            }
        };

        let _ = update_title_from_message(&self.pool, chat_id, &message);
        let _ = add_message(&self.pool, chat_id, "user", &message);
        self.load_messages();

        self.loading = true;
        let pool = self.pool.clone();
        let ollama_url = self.ollama_url.clone();
        let model = self.model.clone();
        let chat_id2 = chat_id;
        let tx = self.tx.clone();

        tokio::spawn(async move {
            Self::run_ollama_loop(ollama_url, model, chat_id2, pool, None, tx).await;
        });
    }

    async fn run_ollama_loop(
        ollama_url: String,
        model: String,
        chat_id: i64,
        pool: DbPool,
        extra_messages: Option<Vec<OllamaChatMessage>>,
        tx: mpsc::UnboundedSender<CliEvent>,
    ) {
        let msgs = if let Some(extra) = extra_messages {
            extra
        } else {
            match build_messages_from_db(&pool, chat_id) {
                Ok(m) => m,
                Err(e) => {
                    let _ = tx.send(CliEvent::Error(e));
                    return;
                }
            }
        };

        let tools = Some(tools::get_tool_definitions());
        match chat_with_ollama(&ollama_url, &model, msgs.clone(), tools).await {
            Ok(resp) => {
                let native_tcs = resp.message.tool_calls.clone().unwrap_or_default();
                let text = &resp.message.content;
                let parsed_tcs = tools::parse_tool_calls_from_text(text);
                let clean_text = tools::strip_tool_calls_from_text(text);

                let tool_calls = if !native_tcs.is_empty() {
                    native_tcs
                } else if !parsed_tcs.is_empty() {
                    parsed_tcs
                } else {
                    Vec::new()
                };

                if !clean_text.is_empty() {
                    let pool2 = pool.clone();
                    let ct = clean_text.clone();
                    tokio::task::spawn_blocking(move || {
                        add_message(&pool2, chat_id, "assistant", &ct).ok();
                    })
                    .await
                    .ok();
                }

                if tool_calls.is_empty() {
                    let _ = tx.send(CliEvent::TextReady);
                } else {
                    let mut context = msgs;
                    context.push(OllamaChatMessage {
                        role: "assistant".to_string(),
                        content: clean_text.clone(),
                        tool_calls: Some(tool_calls.clone()),
                        name: None,
                    });
                    let _ = tx.send(CliEvent::ToolCalls(tool_calls, context));
                }
            }
            Err(e) => {
                let _ = tx.send(CliEvent::Error(e));
            }
        }
    }

    fn confirm_tool(&mut self) {
        let (tool_calls, messages) = match self.pending_tool_calls.take() {
            Some(v) => v,
            None => return,
        };
        self.waiting_confirmation = false;
        self.loading = true;

        let mut extra = messages;
        for tc in &tool_calls {
            let result = match tools::execute_tool(tc) {
                Ok(res) => res,
                Err(e) => format!("Error: {e}"),
            };
            extra.push(OllamaChatMessage {
                role: "tool".to_string(),
                content: result,
                tool_calls: None,
                name: Some(tc.function.name.clone()),
            });
        }

        let pool = self.pool.clone();
        let ollama_url = self.ollama_url.clone();
        let model = self.model.clone();
        let chat_id = self.active_chat_id.unwrap();
        let tx = self.tx.clone();

        tokio::spawn(async move {
            Self::run_ollama_loop(ollama_url, model, chat_id, pool, Some(extra), tx).await;
        });
    }

    fn deny_tool(&mut self) {
        let (_tool_calls, messages) = match self.pending_tool_calls.take() {
            Some(v) => v,
            None => return,
        };
        self.waiting_confirmation = false;
        self.loading = true;

        let mut extra = messages;
        extra.push(OllamaChatMessage {
            role: "system".to_string(),
            content: "The user declined to execute the tool calls. Do not repeat the same request. \
                      Respond as best you can without the tool."
                .to_string(),
            tool_calls: None,
            name: None,
        });

        let pool = self.pool.clone();
        let ollama_url = self.ollama_url.clone();
        let model = self.model.clone();
        let chat_id = self.active_chat_id.unwrap();
        let tx = self.tx.clone();

        tokio::spawn(async move {
            Self::run_ollama_loop(ollama_url, model, chat_id, pool, Some(extra), tx).await;
        });
    }
}

fn init_terminal() -> io::Result<Terminal<impl ratatui::backend::Backend>> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout());
    Terminal::new(backend)
}

fn restore_terminal() -> io::Result<()> {
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

fn draw_sidebar(frame: &mut Frame, area: Rect, app: &App) {
    let constraints = vec![Constraint::Min(1), Constraint::Length(1)];
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let items: Vec<ListItem> = app
        .chats
        .iter()
        .enumerate()
        .map(|(i, chat)| {
            let prefix = if Some(chat.id) == app.active_chat_id {
                " \u{25b6} "
            } else {
                "   "
            };
            let label = if chat.title.len() > 22 {
                format!("{}{}", prefix, &chat.title[..22])
            } else {
                format!("{}{}", prefix, chat.title)
            };
            let style = if i == app.sidebar_index && matches!(app.focus, Focus::Sidebar) {
                Style::default().bg(Color::DarkGray)
            } else if Some(chat.id) == app.active_chat_id {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            ListItem::new(label).style(style)
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().title(" Chats ").borders(Borders::ALL))
        .highlight_style(Style::default().bg(Color::DarkGray));

    let mut state = ratatui::widgets::ListState::default().with_selected(Some(app.sidebar_index));
    frame.render_stateful_widget(list, chunks[0], &mut state);

    let help_text = if matches!(app.focus, Focus::Sidebar) {
        " [n] new  [d] del  [Tab] focus  [q] quit "
    } else {
        " [Tab] focus  [q] quit "
    };
    let help = Paragraph::new(Line::from(Span::styled(
        help_text,
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(help, chunks[1]);
}

fn draw_main(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(7)])
        .split(area);

    let title = match app.active_chat_id {
        Some(id) => {
            let t = app
                .chats
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.title.as_str())
                .unwrap_or("Chat");
            format!(" {t} ")
        }
        None => " Select a chat or press n to create one ".to_string(),
    };

    let mut lines: Vec<Line> = Vec::new();

    if app.messages.is_empty() && app.active_chat_id.is_some() && !app.loading && !app.waiting_confirmation {
        let empty = Paragraph::new("No messages yet. Type below to start chatting.")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().title(title).borders(Borders::ALL));
        frame.render_widget(empty, chunks[0]);
    } else if app.active_chat_id.is_some() {
        for msg in &app.messages {
            let label = match msg.role.as_str() {
                "user" => "You",
                "assistant" => "AI",
                r => r,
            };
            let color = match msg.role.as_str() {
                "user" => Color::Green,
                "assistant" => Color::Cyan,
                _ => Color::White,
            };
            lines.push(Line::from(Span::styled(
                format!("[{label}]"),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(strip_code_blocks(&msg.content)));
            lines.push(Line::from(""));
        }

        if app.waiting_confirmation {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "━━━ AI wants to use tools ━━━",
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )));
            if let Some((tool_calls, _)) = &app.pending_tool_calls {
                for tc in tool_calls {
                    for line in tools::tool_call_description(tc).lines() {
                        lines.push(Line::from(Span::styled(
                            format!("  {line}"),
                            Style::default().fg(Color::Cyan),
                        )));
                    }
                }
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                " Execute? (y/N) ",
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            )));
        }

        if app.loading {
            lines.push(Line::from(Span::styled(
                "[AI] Thinking...",
                Style::default().fg(Color::Yellow),
            )));
        }

        let messages = Paragraph::new(Text::from(lines))
            .block(Block::default().title(title).borders(Borders::ALL))
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0));
        frame.render_widget(messages, chunks[0]);
    } else {
        let empty = Paragraph::new("Select a chat from the sidebar or press n to create one.")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().title(title).borders(Borders::ALL));
        frame.render_widget(empty, chunks[0]);
    }

    let input_style = if matches!(app.focus, Focus::Input) {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let input_text: &str = if app.input.is_empty() {
        " Type a message..."
    } else {
        app.input.as_str()
    };
    let input = Paragraph::new(input_text)
        .style(input_style)
        .block(Block::default().title(" Input ").borders(Borders::ALL));
    frame.render_widget(input, chunks[1]);

    if matches!(app.focus, Focus::Input) {
        let cursor_col = app.input.chars().count() as u16 + 1;
        frame.set_cursor_position((chunks[1].x + cursor_col, chunks[1].y + 1));
    }
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let help_lines = vec![
        Line::from(""),
        Line::from(" pj — Key Bindings").bold(),
        Line::from(""),
        Line::from("  Tab          Switch focus (sidebar / input)"),
        Line::from("  Up / Down    Navigate chat list"),
        Line::from("  Enter        Select chat / Send message"),
        Line::from("  n            New chat"),
        Line::from("  d            Delete active chat"),
        Line::from("  q / Ctrl+c   Quit"),
        Line::from("  ?            Toggle this help"),
        Line::from(""),
        Line::from(" Press any key to close"),
    ];
    let help = Paragraph::new(Text::from(help_lines))
        .style(Style::default().fg(Color::White))
        .block(
            Block::default()
                .title(" Help ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
    frame.render_widget(help, area);
}

fn ui(frame: &mut Frame, app: &App, show_help: bool) {
    if show_help {
        let area = frame.area();
        let help_area = Rect {
            x: area.width / 6,
            y: area.height / 6,
            width: area.width * 2 / 3,
            height: area.height * 2 / 3,
        };
        draw_help(frame, help_area);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(30), Constraint::Min(1)])
        .split(frame.area());

    draw_sidebar(frame, chunks[0], app);
    draw_main(frame, chunks[1], app);

    let top_bar = if app.loading {
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(" pj ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(" Thinking... ", Style::default().fg(Color::Yellow)),
        ]))
    } else if app.waiting_confirmation {
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(" pj ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(" Awaiting confirmation ", Style::default().fg(Color::Green)),
        ]))
    } else {
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(" pj ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(format!(" {} chats ", app.chats.len()), Style::default().fg(Color::DarkGray)),
        ]))
    };
    let top_area = Rect {
        x: chunks[0].x,
        y: chunks[0].y,
        width: chunks[0].width + chunks[1].width,
        height: 1,
    };
    frame.render_widget(top_bar, top_area);
}

fn run_tui(pool: DbPool) -> io::Result<()> {
    let mut terminal = init_terminal()?;
    let mut app = App::new(pool);
    let mut show_help = false;

    terminal.clear()?;

    while !app.exit {
        terminal.draw(|f| ui(f, &mut app, show_help))?;

        if let Ok(event) = app.rx.try_recv() {
            match event {
                CliEvent::TextReady => {
                    app.loading = false;
                    app.load_messages();
                }
                CliEvent::ToolCalls(tcs, msgs) => {
                    app.loading = false;
                    app.load_messages();
                    app.pending_tool_calls = Some((tcs, msgs));
                    app.waiting_confirmation = true;
                }
                CliEvent::Error(e) => {
                    app.loading = false;
                    let _ = add_message(&app.pool, app.active_chat_id.unwrap_or(0), "assistant", &format!("Error: {e}"));
                    app.load_messages();
                }
            }
        }

        let mut socket_events = Vec::new();
        let sp = socket_path();
        if let Some(client) = app.event_client.as_mut() {
            if socket_inode(&sp) != Some(client.ino) {
                app.event_client = None;
            } else {
                loop {
                    match client.try_recv() {
                        Some(Some(change)) => socket_events.push(change),
                        Some(None) => break,
                        None => {
                            app.event_client = None;
                            break;
                        }
                    }
                }
            }
        }
        if app.event_client.is_none() {
            app.event_client = EventClient::connect(&sp);
            if app.event_client.is_some() {
                app.load_chats();
            }
        }
        if !socket_events.is_empty() {
            for ev in &socket_events {
                let ChatChange::Deleted { id } = ev;
                if app.active_chat_id == Some(*id) {
                    app.active_chat_id = None;
                    app.messages = vec![];
                }
            }
            app.load_chats();
        }

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                if show_help {
                    show_help = false;
                    continue;
                }

                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        app.exit = true;
                    }
                    KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                        app.exit = true;
                    }
                    KeyCode::Char('?') if app.focus == Focus::Sidebar => {
                        show_help = !show_help;
                    }
                    _ => {}
                }

                if show_help {
                    continue;
                }

                if app.waiting_confirmation {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            app.confirm_tool();
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                            app.deny_tool();
                        }
                        _ => {}
                    }
                    continue;
                }

                match app.focus {
                    Focus::Sidebar => handle_sidebar_key(&mut app, key),
                    Focus::Input => handle_input_key(&mut app, key),
                }
            }
        }
    }

    restore_terminal()?;
    Ok(())
}

fn handle_sidebar_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if app.sidebar_index > 0 { app.sidebar_index -= 1; }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.sidebar_index + 1 < app.chats.len() { app.sidebar_index += 1; }
        }
        KeyCode::Enter => app.select_chat(app.sidebar_index),
        KeyCode::Char('n') => app.new_chat(),
        KeyCode::Char('d') => {
            if !app.chats.is_empty() && app.chats.len() > app.sidebar_index {
                let id = app.chats[app.sidebar_index].id;
                let _ = delete_chat(&app.pool, id);
                if app.active_chat_id == Some(id) {
                    app.active_chat_id = None;
                    app.messages = vec![];
                }
                app.load_chats();
            }
        }
        KeyCode::Tab => app.focus = Focus::Input,
        _ => {}
    }
}

fn handle_input_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            if app.input.trim() == "/exit" {
                app.input.clear();
                app.exit = true;
            } else {
                app.send_message();
            }
        }
        KeyCode::Tab => app.focus = Focus::Sidebar,
        KeyCode::Char(c) => app.input.push(c),
        KeyCode::Backspace => { app.input.pop(); }
        _ => {}
    }
}

async fn run_one_shot(pool: &DbPool, question: &str) {
    let ollama_url = get_env_or("OLLAMA_URL", "http://localhost:11434");
    let model = get_env_or("MODEL_NAME", "gemma2:9b");

    let chat = match create_chat(pool) {
        Ok(c) => c,
        Err(e) => { eprintln!("Error: {e}"); return; }
    };

    let _ = update_title_from_message(pool, chat.id, question);
    let _ = add_message(pool, chat.id, "user", question);

    println!("You: {question}");
    print!("AI: ");
    io::stdout().flush().ok();

    let current_msgs = match build_messages_from_db(pool, chat.id) {
        Ok(m) => m,
        Err(e) => { eprintln!("\nError: {e}"); return; }
    };

    // Tool loop
    let mut current_msgs = current_msgs;
    let tools = Some(tools::get_tool_definitions());
    loop {
        match chat_with_ollama(&ollama_url, &model, current_msgs.clone(), tools.clone()).await {
            Ok(resp) => {
                let native_tcs = resp.message.tool_calls.clone().unwrap_or_default();
                let text = &resp.message.content;
                let parsed_tcs = tools::parse_tool_calls_from_text(text);
                let clean_text = tools::strip_tool_calls_from_text(text);

                let tool_calls = if !native_tcs.is_empty() {
                    native_tcs
                } else if !parsed_tcs.is_empty() {
                    parsed_tcs
                } else {
                    Vec::new()
                };

                if !clean_text.is_empty() {
                    println!("{}", strip_code_blocks(&clean_text));
                    let _ = add_message(pool, chat.id, "assistant", &clean_text);
                }

                if tool_calls.is_empty() {
                    let blocks = tools::extract_code_blocks(&clean_text);
                    if !blocks.is_empty() {
                        println!("\nThe AI output code blocks instead of creating files.");
                        for (i, block) in blocks.iter().enumerate() {
                            println!("\n--- Code block {} ({}) ---", i + 1, block.language);
                            for line in block.code.lines() {
                                println!("  {line}");
                            }
                            print!("\nEnter filename to create (or 'n' to skip): ");
                            io::stdout().flush().ok();
                            let mut input = String::new();
                            io::stdin().read_line(&mut input).ok();
                            let input = input.trim().to_string();
                            if !input.is_empty() && !input.eq_ignore_ascii_case("n") {
                                let cwd = std::env::current_dir().ok();
                                let path = if input.starts_with('/') {
                                    input.clone()
                                } else if let Some(ref dir) = cwd {
                                    format!("{}/{}", dir.display(), input)
                                } else {
                                    input.clone()
                                };
                                if let Some(parent) = std::path::Path::new(&path).parent() {
                                    let _ = std::fs::create_dir_all(parent);
                                }
                                match std::fs::write(&path, &block.code) {
                                    Ok(_) => println!("  ✅ Created {path}"),
                                    Err(e) => println!("  ❌ Error: {e}"),
                                }
                            }
                        }
                    }
                    break;
                }

                // Save assistant message with tool_calls
                current_msgs.push(OllamaChatMessage {
                    role: "assistant".to_string(),
                    content: clean_text.clone(),
                    tool_calls: Some(tool_calls.clone()),
                    name: None,
                });

                println!();
                for tc in &tool_calls {
                    println!("  ⚡ {}", tools::tool_call_description(tc));
                }
                print!("\nExecute? [y/N] ");
                io::stdout().flush().ok();
                let mut input = String::new();
                io::stdin().read_line(&mut input).ok();

                if input.trim().eq_ignore_ascii_case("y") {
                    for tc in &tool_calls {
                        let result = match tools::execute_tool(tc) {
                            Ok(r) => r,
                            Err(e) => format!("Error: {e}"),
                        };
                        current_msgs.push(OllamaChatMessage {
                            role: "tool".to_string(),
                            content: result,
                            tool_calls: None,
                            name: Some(tc.function.name.clone()),
                        });
                    }
                } else {
                    for tc in &tool_calls {
                        current_msgs.push(OllamaChatMessage {
                            role: "tool".to_string(),
                            content: "User declined to execute.".to_string(),
                            tool_calls: None,
                            name: Some(tc.function.name.clone()),
                        });
                    }
                }
            }
            Err(e) => {
                eprintln!("\nError: {e}");
                break;
            }
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
        if let Err(e) = run_tui(pool) {
            eprintln!("TUI error: {e}");
            let _ = restore_terminal();
        }
    }
}
