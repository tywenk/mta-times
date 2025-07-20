use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::Result;
use directories::ProjectDirs;
use lazy_static::lazy_static;
use tracing_error::ErrorLayer;
use tracing_subscriber::{self, Layer, layer::SubscriberExt, util::SubscriberInitExt};

lazy_static! {
    pub static ref PROJECT_NAME: String = env!("CARGO_CRATE_NAME").to_uppercase().to_string();
    pub static ref DATA_FOLDER: Option<PathBuf> =
        std::env::var(format!("{}_DATA", PROJECT_NAME.clone()))
            .ok()
            .map(PathBuf::from);
    pub static ref LOG_ENV: String = format!("{}_LOGLEVEL", PROJECT_NAME.clone());
}

// Static variable to store the current log file path
static LOG_FILE_PATH: OnceLock<PathBuf> = OnceLock::new();

fn project_directory() -> Option<ProjectDirs> {
    // On MacOs this can be found at ~/Library/Application Support/train-checker/
    // On Windows this can be found at C:\Users\<username>\AppData\Local\train-checker\
    // On Linux this can be found at ~/.local/share/train-checker/
    ProjectDirs::from("com", "train-checker", env!("CARGO_PKG_NAME"))
}

pub fn get_data_dir() -> PathBuf {
    let directory = if let Some(s) = DATA_FOLDER.clone() {
        s
    } else if let Some(proj_dirs) = project_directory() {
        proj_dirs.data_local_dir().to_path_buf()
    } else {
        PathBuf::from(".").join(".data")
    };
    directory
}

pub fn initialize_logging() -> Result<()> {
    let session_id = chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string();
    let directory = get_data_dir();
    std::fs::create_dir_all(directory.clone())?;

    // Create log file name with session_id
    let log_file_name = format!("{}_{}.log", env!("CARGO_PKG_NAME"), session_id);
    let log_path = directory.join(log_file_name);

    // Store the log file path globally
    LOG_FILE_PATH
        .set(log_path.clone())
        .map_err(|_| anyhow::anyhow!("Failed to set log file path - logger already initialized"))?;

    let log_file = std::fs::File::create(log_path)?;

    // Create the environment filter directly instead of setting RUST_LOG
    let env_filter = tracing_subscriber::filter::EnvFilter::try_from_default_env()
        .or_else(|_| {
            // Try the custom log environment variable
            std::env::var(LOG_ENV.clone())
                .map(|level| tracing_subscriber::filter::EnvFilter::new(level))
        })
        .unwrap_or_else(|_| {
            // Default to info level for this crate
            tracing_subscriber::filter::EnvFilter::new(format!("{}=info", env!("CARGO_CRATE_NAME")))
        });

    let file_subscriber = tracing_subscriber::fmt::layer()
        .with_file(true)
        .with_line_number(true)
        .with_writer(log_file)
        .with_target(false)
        .with_ansi(false)
        .with_filter(env_filter);
    tracing_subscriber::registry()
        .with(file_subscriber)
        .with(ErrorLayer::default())
        .init();
    Ok(())
}

/// Gets the current log file path if logging has been initialized
pub fn get_log_file_path() -> Option<&'static PathBuf> {
    LOG_FILE_PATH.get()
}

/// Reads all log entries from the current log file asynchronously
pub async fn read_log_entries() -> Result<Vec<String>> {
    let log_path = get_log_file_path()
        .ok_or_else(|| anyhow::anyhow!("Logging not initialized - no log file path available"))?;

    let contents = tokio::fs::read_to_string(log_path).await?;
    let lines: Vec<String> = contents.lines().map(|line| line.to_string()).collect();

    Ok(lines)
}

/// Similar to the `std::dbg!` macro, but generates `tracing` events rather
/// than printing to stdout.
///
/// By default, the verbosity level for the generated events is `DEBUG`, but
/// this can be customized.
#[macro_export]
macro_rules! trace_dbg {
    (target: $target:expr, level: $level:expr, $ex:expr) => {{
        match $ex {
            value => {
                tracing::event!(target: $target, $level, ?value, stringify!($ex));
                value
            }
        }
    }};
    (level: $level:expr, $ex:expr) => {
        trace_dbg!(target: module_path!(), level: $level, $ex)
    };
    (target: $target:expr, $ex:expr) => {
        trace_dbg!(target: $target, level: tracing::Level::DEBUG, $ex)
    };
    ($ex:expr) => {
        trace_dbg!(level: tracing::Level::DEBUG, $ex)
    };
}
