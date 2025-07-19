use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Wrap},
};
use std::{
    io,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use train_checker::{StopStatus, TrainChecker, TrainCheckerStatus};

#[derive(Debug, Clone)]
enum AppState {
    Loading,
    Selection,
    Polling { stop_id: String, stop_name: String },
}

enum AppEvent {
    TrainCheckerReady(TrainChecker),
    TrainCheckerError(String),
    StopStatusUpdate(StopStatus),
}

struct App {
    state: AppState,
    train_checker: Option<TrainChecker>,

    // Selection state
    stops: Vec<(String, String)>, // (stop_id, stop_name)
    filtered_stops: Vec<usize>,   // indices into stops
    search_input: String,
    list_state: ListState,

    // Polling state
    current_stop_status: Option<StopStatus>,
    polling_interval: Duration,
    last_update: Option<Instant>,

    // UI state
    should_quit: bool,
    error_message: Option<String>,
}

impl App {
    fn new() -> Self {
        Self {
            state: AppState::Loading,
            train_checker: None,
            stops: Vec::new(),
            filtered_stops: Vec::new(),
            search_input: String::new(),
            list_state: ListState::default(),
            current_stop_status: None,
            polling_interval: Duration::from_secs(10),
            last_update: None,
            should_quit: false,
            error_message: None,
        }
    }

    fn handle_key_event(&mut self, key: KeyCode) {
        match &self.state {
            AppState::Loading => {
                // No input during loading
            }
            AppState::Selection => match key {
                KeyCode::Char('q') => self.should_quit = true,
                KeyCode::Enter => {
                    if let Some(selected) = self.list_state.selected() {
                        if selected < self.filtered_stops.len() {
                            let stop_index = self.filtered_stops[selected];
                            let (stop_id, stop_name) = &self.stops[stop_index];
                            self.state = AppState::Polling {
                                stop_id: stop_id.clone(),
                                stop_name: stop_name.clone(),
                            };
                            self.current_stop_status = None;
                            self.last_update = None;
                        }
                    }
                }
                KeyCode::Up => {
                    let selected = self.list_state.selected().unwrap_or(0);
                    if selected > 0 {
                        self.list_state.select(Some(selected - 1));
                    }
                }
                KeyCode::Down => {
                    let selected = self.list_state.selected().unwrap_or(0);
                    if selected + 1 < self.filtered_stops.len() {
                        self.list_state.select(Some(selected + 1));
                    }
                }
                KeyCode::Backspace => {
                    self.search_input.pop();
                    self.filter_stops();
                }
                KeyCode::Char(c) => {
                    self.search_input.push(c);
                    self.filter_stops();
                }
                _ => {}
            },
            AppState::Polling { .. } => {
                match key {
                    KeyCode::Char('q') => self.should_quit = true,
                    KeyCode::Char('s') => {
                        self.state = AppState::Selection;
                        self.current_stop_status = None;
                        self.search_input.clear();
                        self.filter_stops();
                    }
                    KeyCode::Char('+') | KeyCode::Char('=') => {
                        // Decrease polling interval (faster)
                        if self.polling_interval > Duration::from_secs(5) {
                            self.polling_interval =
                                Duration::from_secs((self.polling_interval.as_secs() - 5).max(5));
                        }
                    }
                    KeyCode::Char('-') => {
                        // Increase polling interval (slower)
                        if self.polling_interval < Duration::from_secs(120) {
                            self.polling_interval =
                                Duration::from_secs((self.polling_interval.as_secs() + 5).min(120));
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn handle_app_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::TrainCheckerReady(checker) => {
                let stops: Vec<(String, String)> = checker
                    .get_all_stops()
                    .into_iter()
                    .map(|(id, name)| (id, name.unwrap_or_else(|| "Unknown".to_string())))
                    .collect();

                self.stops = stops;
                self.train_checker = Some(checker);
                self.state = AppState::Selection;
                self.filter_stops();
            }
            AppEvent::TrainCheckerError(error) => {
                self.error_message = Some(error);
            }
            AppEvent::StopStatusUpdate(status) => {
                if matches!(self.state, AppState::Polling { .. }) {
                    self.current_stop_status = Some(status);
                    self.last_update = Some(Instant::now());
                }
            }
        }
    }

    fn filter_stops(&mut self) {
        let search_lower = self.search_input.to_lowercase();

        self.filtered_stops = self
            .stops
            .iter()
            .enumerate()
            .filter(|(_, (stop_id, stop_name))| {
                stop_id.to_lowercase().contains(&search_lower)
                    || stop_name.to_lowercase().contains(&search_lower)
            })
            .map(|(i, _)| i)
            .collect();

        // Reset selection if current selection is no longer valid
        if let Some(selected) = self.list_state.selected() {
            if selected >= self.filtered_stops.len() {
                self.list_state.select(if self.filtered_stops.is_empty() {
                    None
                } else {
                    Some(0)
                });
            }
        } else if !self.filtered_stops.is_empty() {
            self.list_state.select(Some(0));
        }
    }

    fn should_poll(&self) -> bool {
        match &self.state {
            AppState::Polling { .. } => self
                .last_update
                .map(|last| last.elapsed() >= self.polling_interval)
                .unwrap_or(true),
            _ => false,
        }
    }

    fn get_current_stop_id(&self) -> Option<&str> {
        match &self.state {
            AppState::Polling { stop_id, .. } => Some(stop_id),
            _ => None,
        }
    }
}

fn ui(f: &mut Frame, app: &mut App) {
    match &app.state {
        AppState::Loading => render_loading(f, app),
        AppState::Selection => render_selection(f, app),
        AppState::Polling { stop_name, .. } => render_polling(f, app, stop_name),
    }
}

fn render_loading(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Percentage(20),
            Constraint::Percentage(40),
        ])
        .split(f.area());

    let loading_block = Block::default()
        .title("NYC Train Checker")
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::Blue));

    let loading_text = if let Some(error) = &app.error_message {
        Text::from(vec![
            Line::from("Error loading train data:"),
            Line::from(""),
            Line::from(error.as_str()).style(Style::default().fg(Color::Red)),
            Line::from(""),
            Line::from("Press 'q' to quit"),
        ])
    } else {
        Text::from(vec![
            Line::from("Loading GTFS data..."),
            Line::from(""),
            Line::from("This may take a moment on first run."),
        ])
    };

    let loading_paragraph = Paragraph::new(loading_text)
        .block(loading_block)
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(Color::White));

    f.render_widget(loading_paragraph, chunks[1]);

    // Render a simple progress indicator if no error
    if app.error_message.is_none() {
        let progress = Gauge::default()
            .block(Block::default().borders(Borders::ALL))
            .gauge_style(Style::default().fg(Color::Green))
            .percent(50)
            .label("Loading...");

        let progress_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(20),
                Constraint::Percentage(60),
                Constraint::Percentage(20),
            ])
            .split(chunks[2]);

        f.render_widget(progress, progress_chunks[1]);
    }
}

fn render_selection(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Length(3), // Search
            Constraint::Min(0),    // List
            Constraint::Length(3), // Footer
        ])
        .split(f.area());

    // Header
    let header = Paragraph::new("NYC Train Checker - Select a Stop")
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(Color::Blue));
    f.render_widget(header, chunks[0]);

    // Search input
    let search_title = if app.search_input.is_empty() {
        "Search (type to filter)"
    } else {
        "Search (typing...)"
    };
    let search_block = Block::default().title(search_title).borders(Borders::ALL);
    let search_text = if app.search_input.is_empty() {
        "Start typing to search..."
    } else {
        &app.search_input
    };
    let search_paragraph = Paragraph::new(search_text)
        .block(search_block)
        .style(Style::default().fg(Color::Yellow));
    f.render_widget(search_paragraph, chunks[1]);

    // Stop list
    let items: Vec<ListItem> = app
        .filtered_stops
        .iter()
        .map(|&i| {
            let (stop_id, stop_name) = &app.stops[i];
            ListItem::new(format!("{}: {}", stop_id, stop_name))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title(format!(
                    "Stops ({}/{})",
                    app.filtered_stops.len(),
                    app.stops.len()
                ))
                .borders(Borders::ALL),
        )
        .style(Style::default().fg(Color::White))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol(">> ");

    f.render_stateful_widget(list, chunks[2], &mut app.list_state);

    // Footer with instructions
    let footer = Paragraph::new("↑↓: Navigate | Enter: Select | q: Quit")
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(Color::Gray));
    f.render_widget(footer, chunks[3]);
}

fn render_polling(f: &mut Frame, app: &App, stop_name: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(0),    // Main content
            Constraint::Length(5), // Footer with controls
        ])
        .split(f.area());

    // Header
    let header_text = format!("Monitoring: {}", stop_name);
    let header = Paragraph::new(header_text)
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(Color::Green));
    f.render_widget(header, chunks[0]);

    // Main content area
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(chunks[1]);

    // Train arrivals (left side)
    render_train_arrivals(f, app, main_chunks[0]);

    // Status and controls (right side)
    render_status_panel(f, app, main_chunks[1]);

    // Footer with instructions
    let footer_text = format!(
        "Refresh: {} seconds | s: Switch Stop | +/-: Adjust Rate | q: Quit",
        app.polling_interval.as_secs()
    );
    let footer = Paragraph::new(footer_text)
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(Color::Gray));
    f.render_widget(footer, chunks[2]);
}

fn render_train_arrivals(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let block = Block::default()
        .title("Upcoming Trains")
        .borders(Borders::ALL);

    if let Some(status) = &app.current_stop_status {
        if status.train_arrivals.is_empty() {
            let no_trains = Paragraph::new("No upcoming trains found")
                .block(block)
                .style(Style::default().fg(Color::Yellow));
            f.render_widget(no_trains, area);
        } else {
            let mut lines = Vec::new();

            for (route_id, arrivals) in &status.train_arrivals {
                let route_display = arrivals
                    .first()
                    .and_then(|a| a.route_name.as_ref())
                    .unwrap_or(route_id);

                lines.push(Line::from(vec![Span::styled(
                    format!("{} Train", route_display),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )]));

                for (i, arrival) in arrivals.iter().take(2).enumerate() {
                    lines.push(Line::from(format!("  {}: {}", i + 1, arrival.human_time)));
                }
                lines.push(Line::from(""));
            }

            let paragraph = Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(Color::White));
            f.render_widget(paragraph, area);
        }
    } else {
        let loading = Paragraph::new("Loading train data...")
            .block(block)
            .style(Style::default().fg(Color::Yellow));
        f.render_widget(loading, area);
    }
}

fn render_status_panel(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    // Status info
    let mut status_lines = Vec::new();

    if let Some(checker) = &app.train_checker {
        let status = checker.get_status();
        let failed_requests = checker.get_failed_requests_count();

        status_lines.push(Line::from(format!("Failed requests: {}", failed_requests)));

        let (status_text, status_color) = match status {
            TrainCheckerStatus::Ok => ("OK", Color::Green),
            TrainCheckerStatus::Error => ("ERROR", Color::Red),
        };

        status_lines.push(Line::from(vec![
            Span::raw("Status: "),
            Span::styled(status_text, Style::default().fg(status_color)),
        ]));
    }

    if let Some(last_update) = app.last_update {
        let elapsed = last_update.elapsed().as_secs();
        status_lines.push(Line::from(format!("Last update: {}s ago", elapsed)));
    }

    let status_block = Block::default()
        .title("System Status")
        .borders(Borders::ALL);

    let status_paragraph = Paragraph::new(status_lines)
        .block(status_block)
        .style(Style::default().fg(Color::White));
    f.render_widget(status_paragraph, chunks[0]);

    // Polling controls
    let control_lines = vec![
        Line::from("Controls:"),
        Line::from(""),
        Line::from("+ : Faster refresh"),
        Line::from("- : Slower refresh"),
        Line::from(format!("Rate: {}s", app.polling_interval.as_secs())),
    ];

    let control_block = Block::default()
        .title("Polling Controls")
        .borders(Borders::ALL);

    let control_paragraph = Paragraph::new(control_lines)
        .block(control_block)
        .style(Style::default().fg(Color::White));
    f.render_widget(control_paragraph, chunks[1]);
}

async fn run_app() -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app state
    let mut app = App::new();

    // Create event channels
    let (tx, mut rx) = mpsc::unbounded_channel();

    // Spawn TrainChecker initialization
    let init_tx = tx.clone();
    tokio::spawn(async move {
        match TrainChecker::new().await {
            Ok(checker) => {
                if let Err(_) = init_tx.send(AppEvent::TrainCheckerReady(checker)) {
                    // Channel closed, app probably quit
                }
            }
            Err(e) => {
                if let Err(_) = init_tx.send(AppEvent::TrainCheckerError(e.to_string())) {
                    // Channel closed, app probably quit
                }
            }
        }
    });

    // We'll handle input directly in the main loop rather than spawning a separate task
    // This prevents conflicts between the spawned handler and main loop event polling

    // Main app loop
    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        // Handle keyboard input in a non-blocking way
        // Use a short timeout to keep the app responsive
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    app.handle_key_event(key.code);
                }
                Event::Resize(_, _) => {
                    // Terminal was resized, will be handled in next draw
                }
                _ => {
                    // Ignore other events
                }
            }
        }

        // Handle async events (TrainChecker initialization, etc.)
        // Use a very short timeout to not block the UI
        match tokio::time::timeout(Duration::from_millis(10), rx.recv()).await {
            Ok(Some(event)) => {
                app.handle_app_event(event);
            }
            Ok(None) => {
                // Channel closed, should quit
                break;
            }
            Err(_) => {
                // Timeout, continue
            }
        }

        // Handle polling
        if app.should_poll() {
            if let (Some(checker), Some(stop_id)) = (&app.train_checker, app.get_current_stop_id())
            {
                let stop_id = stop_id.to_string();

                // Handle polling synchronously for now
                // In a production app, we'd want to restructure this to use Arc<Mutex<TrainChecker>>
                match checker.get_stop_status(&stop_id).await {
                    Ok(status) => {
                        app.handle_app_event(AppEvent::StopStatusUpdate(status));
                    }
                    Err(_) => {
                        // Silently ignore polling errors for now
                    }
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    run_app().await
}
