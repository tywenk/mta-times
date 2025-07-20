use anyhow::Result;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::info;
use train_checker::{StopStatus, TrainChecker, TrainCheckerStatus};
use tui_big_text::{BigText, PixelSize};

mod logger;

#[derive(Debug, Clone)]
enum AppState {
    Loading,
    Selection,
    Polling { stop_id: String, stop_name: String },
    Log,
}

enum AppEvent {
    TrainCheckerReady(TrainChecker),
    TrainCheckerError(String),
    StopStatusUpdate(StopStatus),
    LogFileLoaded,
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
    previous_state: Option<AppState>,

    // Log state
    file_log_entries: Vec<String>, // Logs read from file
    needs_log_reload: bool,        // Flag to trigger log file reload
}

impl App {
    fn new() -> Self {
        let app = Self {
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
            previous_state: None,
            file_log_entries: Vec::new(),
            needs_log_reload: false,
        };
        app.log("Application initialized".to_string());
        app
    }

    fn log(&self, message: String) {
        // Log to tracing (will write to file)
        info!("{}", message);
    }

    /// Load logs from the file and update the file_log_entries field
    async fn update_file_logs(&mut self) -> Result<()> {
        match logger::read_log_entries().await {
            Ok(logs) => {
                self.file_log_entries = logs;
                self.log("Loaded logs from file".to_string());
                Ok(())
            }
            Err(e) => {
                self.log(format!("Failed to read logs from file: {}", e));
                Err(e)
            }
        }
    }

    fn handle_key_event(&mut self, key: crossterm::event::KeyEvent) {
        // Handle Ctrl-C globally to quit
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        match &self.state {
            AppState::Loading => {}
            AppState::Selection => match key.code {
                KeyCode::Enter => {
                    if let Some(selected) = self.list_state.selected() {
                        if selected < self.filtered_stops.len() {
                            let stop_index = self.filtered_stops[selected];
                            let (stop_id, display_name) = &self.stops[stop_index];
                            self.log(format!("Selected stop: {} ({})", display_name, stop_id));
                            self.state = AppState::Polling {
                                stop_id: stop_id.clone(),
                                stop_name: display_name.clone(),
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
                KeyCode::Char('l') | KeyCode::Char('L') => {
                    self.log("Entering log mode from selection".to_string());
                    self.previous_state = Some(self.state.clone());
                    self.state = AppState::Log;
                    self.needs_log_reload = true; // Trigger log file reload
                }
                KeyCode::Char(c) => {
                    self.search_input.push(c);
                    self.filter_stops();
                }
                _ => {}
            },
            AppState::Polling { .. } => {
                match key.code {
                    KeyCode::Char('s') => {
                        self.log("Returning to stop selection".to_string());
                        self.state = AppState::Selection;
                        self.current_stop_status = None;
                        self.search_input.clear();
                        self.filter_stops();
                    }
                    KeyCode::Char('l') | KeyCode::Char('L') => {
                        self.log("Entering log mode from polling".to_string());
                        self.previous_state = Some(self.state.clone());
                        self.state = AppState::Log;
                        self.needs_log_reload = true; // Trigger log file reload
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
            AppState::Log => {
                match key.code {
                    KeyCode::Char('l') | KeyCode::Esc => {
                        // Exit log mode and return to previous state
                        let previous = self.previous_state.clone().unwrap_or(AppState::Selection);
                        self.log(format!("Exiting log mode, returning to {:?}", previous));
                        self.state = previous;
                        self.previous_state = None;
                    }
                    _ => {
                        // In log mode, most keys are ignored (Ctrl-C handled globally above)
                    }
                }
            }
        }
    }

    fn handle_app_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::TrainCheckerReady(checker) => {
                self.log("TrainChecker ready, loading stops".to_string());
                let stops: Vec<(String, String)> = checker
                    .get_all_stops()
                    .into_iter()
                    .filter_map(|(id, name)| {
                        // Only include stops that end with 'N' or 'S'
                        if id.ends_with('N') || id.ends_with('S') {
                            let stop_name = name.unwrap_or_else(|| "Unknown".to_string());
                            let display_name = checker.format_stop_display(&id, &stop_name);
                            Some((id, display_name))
                        } else {
                            None
                        }
                    })
                    .collect();

                self.log(format!("Loaded {} stops", stops.len()));
                self.stops = stops;
                self.train_checker = Some(checker);
                self.state = AppState::Selection;
                self.filter_stops();
            }
            AppEvent::TrainCheckerError(error) => {
                self.log(format!("TrainChecker error: {}", error));
                self.error_message = Some(error);
            }
            AppEvent::StopStatusUpdate(status) => {
                if matches!(self.state, AppState::Polling { .. }) {
                    self.log(format!(
                        "Updated stop status: {} train arrivals",
                        status.train_arrivals.len()
                    ));
                    self.current_stop_status = Some(status);
                    self.last_update = Some(Instant::now());
                }
            }
            AppEvent::LogFileLoaded => {
                // Log file has been loaded, UI will automatically update
            }
        }
    }

    fn filter_stops(&mut self) {
        let search_lower = self.search_input.to_lowercase();

        self.filtered_stops = self
            .stops
            .iter()
            .enumerate()
            .filter(|(_, (stop_id, display_name))| {
                stop_id.to_lowercase().contains(&search_lower)
                    || display_name.to_lowercase().contains(&search_lower)
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

    async fn run(mut self, mut terminal: DefaultTerminal) -> Result<()> {
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

        // Simple event loop - let ratatui handle efficiency
        loop {
            // Always draw - ratatui only updates what changed
            terminal.draw(|f| self.draw(f))?;

            // Handle events with reasonable timeout
            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        self.handle_key_event(key);
                    }
                    Event::Resize(_, _) => {
                        // Ratatui handles this automatically
                    }
                    _ => {}
                }
            }

            // Handle async events
            match tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
                Ok(Some(event)) => {
                    self.handle_app_event(event);
                }
                Ok(None) => break, // Channel closed
                Err(_) => {}       // Timeout, continue
            }

            // Handle polling
            if self.should_poll() {
                if let (Some(checker), Some(stop_id)) =
                    (&self.train_checker, self.get_current_stop_id())
                {
                    let stop_id = stop_id.to_string();
                    match checker.get_stop_status(&stop_id).await {
                        Ok(status) => {
                            self.handle_app_event(AppEvent::StopStatusUpdate(status));
                        }
                        Err(_) => {} // Silently ignore polling errors
                    }
                }
            }

            // Handle log file loading
            if self.needs_log_reload {
                self.needs_log_reload = false;
                if let Err(_) = self.update_file_logs().await {
                    // Error already logged in update_file_logs
                    self.log("Error reloading log file".to_string());
                }
                // Send event to trigger UI update (though it's not strictly necessary)
                if let Err(_) = tx.send(AppEvent::LogFileLoaded) {
                    // Channel closed, app probably quitting
                    self.log("Channel closed, app probably quitting".to_string());
                }
            }

            if self.should_quit {
                break;
            }
        }

        Ok(())
    }

    fn draw(&mut self, f: &mut Frame) {
        match &self.state {
            AppState::Loading => render_loading(f, self),
            AppState::Log => render_log(f, self),
            AppState::Selection => render_selection(f, self),
            AppState::Polling { stop_name, .. } => render_polling(f, self, stop_name),
        }
    }
}

fn render_loading(f: &mut Frame, app: &App) {
    if let Some(error) = &app.error_message {
        // Show error in a traditional text block
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(40),
                Constraint::Percentage(20),
                Constraint::Percentage(40),
            ])
            .split(f.area());

        let error_block = Block::default()
            .title("NYC Train Checker - Error")
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::Red));

        let error_text = Text::from(vec![
            Line::from("Error loading train data:"),
            Line::from(""),
            Line::from(error.as_str()).style(Style::default().fg(Color::Red)),
            Line::from(""),
            Line::from("Press 'Ctrl-C' to quit"),
        ]);

        let error_paragraph = Paragraph::new(error_text)
            .block(error_block)
            .wrap(Wrap { trim: true })
            .style(Style::default().fg(Color::White));

        f.render_widget(error_paragraph, chunks[1]);
    } else {
        // Show big "LOADING" text
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(30),
                Constraint::Length(8), // Height for big text
                Constraint::Length(3), // Height for subtitle
                Constraint::Percentage(30),
            ])
            .split(f.area());

        // Big "LOADING" text
        let big_text = BigText::builder()
            .pixel_size(PixelSize::Full)
            .style(Style::default().fg(Color::Blue))
            .lines(vec!["LOADING".into()])
            .centered()
            .build();

        f.render_widget(big_text, chunks[1]);

        // Subtitle text
        let subtitle = Paragraph::new("NYC Train Checker - Fetching GTFS data...")
            .style(Style::default().fg(Color::Gray))
            .block(Block::default().borders(Borders::NONE))
            .alignment(Alignment::Center);

        f.render_widget(subtitle, chunks[2]);
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
            let (_stop_id, display_name) = &app.stops[i];
            ListItem::new(display_name.as_str())
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
    let footer = Paragraph::new("↑↓: Navigate | Enter: Select | Ctrl-C: Quit")
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(Color::Gray));
    f.render_widget(footer, chunks[3]);
}

fn render_polling(f: &mut Frame, app: &App, stop_name: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(0),    // Train arrivals (full width)
            Constraint::Length(3), // Bottom bar with status and controls
        ])
        .split(f.area());

    // Header
    let header_text = format!("Monitoring: {}", stop_name);
    let header = Paragraph::new(header_text)
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(Color::Green));
    f.render_widget(header, chunks[0]);

    // Train arrivals (full width)
    render_train_arrivals(f, app, chunks[1]);

    // Bottom bar with status and controls
    render_bottom_bar(f, app, chunks[2]);
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

fn render_bottom_bar(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    // Create the footer text with current rate
    let footer_text = format!(
        "Rate: {}s | s: Switch Stop | +/-: Adjust Rate | Ctrl-L: Log | Ctrl-C: Quit",
        app.polling_interval.as_secs()
    );

    // Create status text
    let mut status_text = String::new();
    if let Some(checker) = &app.train_checker {
        let status = checker.get_status();
        let failed_requests = checker.get_failed_requests_count();

        let status_symbol = match status {
            TrainCheckerStatus::Ok => "OK",
            TrainCheckerStatus::Error => "ERR",
        };

        if failed_requests > 0 {
            status_text = format!("{}:{}", status_symbol, failed_requests);
        } else {
            status_text = status_symbol.to_string();
        }

        if let Some(last_update) = app.last_update {
            let elapsed = last_update.elapsed().as_secs();
            status_text.push_str(&format!(" ({}s ago)", elapsed));
        }
    }

    // Calculate layout - status is right-aligned with its content width
    let status_width = status_text.len() as u16 + 2; // +2 for borders
    let footer_width = area.width.saturating_sub(status_width);

    let bottom_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(footer_width),
            Constraint::Length(status_width),
        ])
        .split(area);

    // Render footer
    let footer = Paragraph::new(footer_text)
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(Color::Gray));
    f.render_widget(footer, bottom_chunks[0]);

    // Render status (right-aligned)
    let status_color = if let Some(checker) = &app.train_checker {
        match checker.get_status() {
            TrainCheckerStatus::Ok => Color::Green,
            TrainCheckerStatus::Error => Color::Red,
        }
    } else {
        Color::Gray
    };

    let status = Paragraph::new(status_text)
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(status_color));
    f.render_widget(status, bottom_chunks[1]);
}

fn render_log(f: &mut Frame, app: &App) {
    let main_block = Block::default()
        .title("Debug Log (from file) (L to enter, q/Esc/Ctrl-L to exit, Ctrl-C to quit)")
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::White));

    let log_messages = if app.file_log_entries.is_empty() {
        vec![ListItem::new("No log entries found or logs not yet loaded")]
    } else {
        // Calculate how many lines we can display (subtract 2 for borders)
        let display_height = f.area().height.saturating_sub(2) as usize;
        let max_lines = display_height.max(10); // Ensure at least 10 lines

        // Take the last N lines (most recent) and display in chronological order
        // This way newest logs appear at the bottom
        let start_index = app.file_log_entries.len().saturating_sub(max_lines);
        app.file_log_entries
            .iter()
            .skip(start_index)
            .map(|msg| ListItem::new(msg.clone()))
            .collect::<Vec<_>>()
    };

    let log_list = List::new(log_messages)
        .block(main_block)
        .style(Style::default().fg(Color::White))
        .highlight_style(Style::default().bg(Color::Blue));

    f.render_widget(log_list, f.area());
}

async fn run_app() -> Result<()> {
    logger::initialize_logging()?;
    info!("Starting train checker application");

    let terminal = ratatui::init();

    let app_result = App::new().run(terminal).await;
    ratatui::restore();
    app_result
}

#[tokio::main]
async fn main() -> Result<()> {
    run_app().await
}
