use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use personal_jesus::*;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::io::{self, stdout, Write};
use tokio::sync::mpsc;

enum Focus {
    Sidebar,
    Input,
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
    tx: mpsc::UnboundedSender<()>,
    rx: mpsc::UnboundedReceiver<()>,
}

impl App {
    fn new(pool: DbPool) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let ollama_url = get_env_or("OLLAMA_URL", "http://localhost:11434");
        let model = get_env_or("MODEL_NAME", "qwen2.5:7b");
        let mut app = Self {
            chats: vec![],
            messages: vec![],
            active_chat_id: None,
            input: String::new(),
            loading: false,
            sidebar_index: 0,
            focus: Focus::Sidebar,
            scroll: 0,
            exit: false,
            pool,
            ollama_url,
            model,
            tx,
            rx,
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
            let response = query_ollama(&ollama_url, &model, &message)
                .await
                .unwrap_or_else(|e| format!("Error: {e}"));
            tokio::task::spawn_blocking(move || {
                let _ = add_message(&pool, chat_id2, "assistant", &response);
            })
            .await
            .ok();
            let _ = tx.send(());
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
    let constraints = if app.chats.is_empty() {
        vec![Constraint::Min(1), Constraint::Length(1)]
    } else {
        vec![Constraint::Min(1), Constraint::Length(1)]
    };
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
        .constraints([Constraint::Min(1), Constraint::Length(3)])
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

    if app.messages.is_empty() && app.active_chat_id.is_some() && !app.loading {
        let empty = Paragraph::new("No messages yet. Type below to start chatting.")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().title(title).borders(Borders::ALL));
        frame.render_widget(empty, chunks[0]);
    } else if app.active_chat_id.is_some() {
        let mut lines: Vec<Line> = Vec::new();
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
            lines.push(Line::from(msg.content.as_str()));
            lines.push(Line::from(""));
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
    let input_text: &str = if app.input.is_empty() && matches!(app.focus, Focus::Sidebar) {
        " Type a message..."
    } else {
        app.input.as_str()
    };
    let input = Paragraph::new(input_text)
        .style(input_style)
        .block(Block::default().title(" Input ").borders(Borders::ALL));
    frame.render_widget(input, chunks[1]);

    if matches!(app.focus, Focus::Input) {
        let chars: Vec<char> = app.input.chars().collect();
        let cursor_col = if chars.is_empty() {
            1
        } else {
            chars.len() as u16 + 1
        };
        frame.set_cursor_position((
            chunks[1].x + cursor_col,
            chunks[1].y + 1,
        ));
    }
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let help_lines = vec![
        Line::from(""),
        Line::from(" Personal Jesus — Key Bindings").bold(),
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
            Span::styled(
                " Personal Jesus ",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(" Thinking... ", Style::default().fg(Color::Yellow)),
        ]))
    } else {
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                " Personal Jesus ",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                format!(" {} chats ", app.chats.len()),
                Style::default().fg(Color::DarkGray),
            ),
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

        if let Ok(()) = app.rx.try_recv() {
            app.loading = false;
            app.load_messages();
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
                    KeyCode::Char('q') if key.modifiers == KeyModifiers::NONE => {
                        app.exit = true;
                    }
                    KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                        app.exit = true;
                    }
                    KeyCode::Char('?') => {
                        show_help = !show_help;
                    }
                    _ => {}
                }

                if show_help {
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
            if app.sidebar_index > 0 {
                app.sidebar_index -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.sidebar_index + 1 < app.chats.len() {
                app.sidebar_index += 1;
            }
        }
        KeyCode::Enter => {
            app.select_chat(app.sidebar_index);
        }
        KeyCode::Char('n') => {
            app.new_chat();
        }
        KeyCode::Char('d') => {
            if !app.chats.is_empty() {
                if app.chats.len() > app.sidebar_index {
                    let id = app.chats[app.sidebar_index].id;
                    let _ = delete_chat(&app.pool, id);
                    if app.active_chat_id == Some(id) {
                        app.active_chat_id = None;
                        app.messages = vec![];
                    }
                    app.load_chats();
                }
            }
        }
        KeyCode::Tab => {
            app.focus = Focus::Input;
        }
        _ => {}
    }
}

fn handle_input_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            app.send_message();
        }
        KeyCode::Tab | KeyCode::Esc => {
            app.focus = Focus::Sidebar;
        }
        KeyCode::Char(c) => {
            app.input.push(c);
        }
        KeyCode::Backspace => {
            app.input.pop();
        }
        _ => {}
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

    let _ = update_title_from_message(pool, chat.id, question);
    let _ = add_message(pool, chat.id, "user", question);

    println!("You: {question}");

    print!("AI: ");
    io::stdout().flush().ok();

    match query_ollama(&ollama_url, &model, question).await {
        Ok(response) => {
            println!("{response}");
            let _ = add_message(pool, chat.id, "assistant", &response);
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
        if let Err(e) = run_tui(pool) {
            eprintln!("TUI error: {e}");
            let _ = restore_terminal();
        }
    }
}
