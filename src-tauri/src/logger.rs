use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use chrono::Local;
use serde::{Deserialize, Serialize};
use tauri::command;
use std::sync::Once;
use serde_json::Value as JsonValue;
use std::sync::mpsc::{channel, Sender, Receiver};
use std::thread;
use std::time::{Duration, Instant};

// Safe println that can be used when the logger itself has an error
// This is a critical fallback and should only be used when the logger fails
fn safe_println(message: &str) {
    // Use eprintln to ensure it goes to stderr for error messages
    eprintln!("[LOGGER_ERROR] {}", message);
}

// Lazily initialized static logger
static LOGGER: Mutex<Option<Logger>> = Mutex::new(None);
static INIT: Once = Once::new();

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
    Critical,
}

impl From<LogLevel> for sentry::Level {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Debug => sentry::Level::Debug,
            LogLevel::Info => sentry::Level::Info,
            LogLevel::Warning => sentry::Level::Warning,
            LogLevel::Error => sentry::Level::Error,
            LogLevel::Critical => sentry::Level::Fatal,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LogEntry {
    timestamp: String,
    level: LogLevel,
    message: String,
    source: String,
    details: Option<String>,
}

struct Logger {
    log_file_path: PathBuf,
    enabled: bool,
    sender: Sender<LogEntry>,
}

impl Logger {
    fn new(log_file_path: PathBuf) -> Self {
        // Create a channel for sending log entries to the background thread
        let (sender, receiver) = channel::<LogEntry>();
        
        // Spawn a background thread for writing logs
        let log_path = log_file_path.clone();
        thread::spawn(move || {
            Logger::background_logger(log_path, receiver);
        });
        
        Self {
            log_file_path,
            enabled: true,
            sender,
        }
    }
    
    // Background thread function that handles actual file I/O
    fn background_logger(log_file_path: PathBuf, receiver: Receiver<LogEntry>) {
        let mut buffer = Vec::new();
        let mut last_flush = Instant::now();
        let flush_interval = Duration::from_millis(500); // Flush every 500ms
        let buffer_size_limit = 50; // Or flush when buffer reaches this size
        
        loop {
            // Check if there's a message with a timeout
            match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(entry) => {
                    // Format the log entry
                    let log_line = format!(
                        "[{}] [{}] [{}] {}{}\n",
                        entry.timestamp,
                        format!("{:?}", entry.level).to_uppercase(),
                        entry.source,
                        entry.message,
                        entry.details.as_ref().map_or(String::new(), |d| format!(": {}", d))
                    );
                    
                    // Add to buffer
                    buffer.push(log_line);
                    
                    // Check if buffer is full
                    if buffer.len() >= buffer_size_limit {
                        Logger::flush_buffer(&log_file_path, &mut buffer);
                        last_flush = Instant::now();
                    }
                },
                Err(_) => {
                    // No message received within timeout, check if we should flush due to time
                    if !buffer.is_empty() && last_flush.elapsed() >= flush_interval {
                        Logger::flush_buffer(&log_file_path, &mut buffer);
                        last_flush = Instant::now();
                    }
                }
            }
        }
    }
    
    // Helper function to write buffered logs to file
    fn flush_buffer(log_file_path: &PathBuf, buffer: &mut Vec<String>) {
        if buffer.is_empty() {
            return;
        }
        
        // Use a scope to ensure file handle is dropped after use
        let result: Result<(), io::Error> = (|| {
            // Double-check that the parent directory exists
            if let Some(parent) = log_file_path.parent() {
                if !parent.exists() {
                    safe_println(&format!("Creating parent directory: {:?}", parent));
                    std::fs::create_dir_all(parent)?;
                }
            }
            
            // Try to open the file with detailed error handling
            let file_result = OpenOptions::new()
                .write(true)
                .append(true)
                .create(true)
                .open(log_file_path);
            
            let mut file = match file_result {
                Ok(f) => f,
                Err(e) => {
                    safe_println(&format!("Error opening log file {:?}: {}", log_file_path, e));
                    // Additional diagnostics
                    if let Some(parent) = log_file_path.parent() {
                        if !parent.exists() {
                            safe_println(&format!("Parent directory doesn't exist: {:?}", parent));
                        } else {
                            safe_println(&format!("Parent directory exists but file open failed"));
                        }
                    }
                    return Err(e);
                }
            };
            
            // Write all buffered entries at once
            for log_line in buffer.iter() {
                match file.write_all(log_line.as_bytes()) {
                    Ok(_) => {},
                    Err(e) => {
                        safe_println(&format!("Error writing to log file: {}", e));
                        return Err(e);
                    }
                }
            }
            
            // Ensure data is flushed to disk
            if let Err(e) = file.flush() {
                safe_println(&format!("Error flushing log file: {}", e));
                return Err(e);
            }
            
            Ok(())
        })();
        
        // Handle errors but don't crash
        if let Err(e) = result {
            safe_println(&format!("Failed to write to log file: {}", e));
            
            // Additional diagnostics for common file errors
            match e.kind() {
                io::ErrorKind::PermissionDenied => {
                    safe_println("Permission denied when writing to log file. Check folder permissions.");
                },
                io::ErrorKind::NotFound => {
                    safe_println("Log file path not found. Directory might not exist.");
                },
                _ => {
                    safe_println(&format!("IO error of kind: {:?}", e.kind()));
                }
            }
        } else {
            // Only print success occasionally to avoid spamming
            static mut SUCCESS_COUNT: usize = 0;
            unsafe {
                SUCCESS_COUNT += 1;
                if SUCCESS_COUNT % 10 == 1 { // Only print every 10th success
                    safe_println(&format!("Successfully wrote {} log entries to file", buffer.len()));
                }
            }
        }
        
        // Clear the buffer regardless of write success
        buffer.clear();
    }
    
    fn log(&self, entry: &LogEntry) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        
        // Clone the entry and send it to the background thread
        let entry_clone = entry.clone();
        if let Err(e) = self.sender.send(entry_clone) {
            return Err(format!("Failed to queue log entry: {}", e));
        }
        
        // Immediate return as the actual writing happens in background
        Ok(())
    }
}

// Initialize the logger
pub fn init_logger(_app_data_dir: &PathBuf) {
    INIT.call_once(|| {
        // Use the current directory instead of app_data_dir
        let current_dir = match std::env::current_dir() {
            Ok(dir) => dir,
            Err(e) => {
                safe_println(&format!("Failed to get current directory: {}", e));
                // Fallback to a reasonable default
                PathBuf::from(".")
            }
        };
        
        let log_dir = current_dir.join("logs");
        
        // Create the logs directory if it doesn't exist
        if let Err(e) = std::fs::create_dir_all(&log_dir) {
            safe_println(&format!("Failed to create log directory at {:?}: {}", log_dir, e));
            // Continue anyway, the open operation will fail gracefully
        } else {
            safe_println(&format!("Created or confirmed log directory at {:?}", log_dir));
        }
        
        let log_file = log_dir.join(format!("free-discord-{}.log", 
            Local::now().format("%Y-%m-%d")));
        
        safe_println(&format!("Initializing logger with log file at {:?}", log_file));
        
        let logger = Logger::new(log_file);
        
        // Set the logger - handle potential mutex poisoning
        match LOGGER.lock() {
            Ok(mut guard) => {
                *guard = Some(logger);
                safe_println("Logger successfully initialized");
            },
            Err(e) => {
                safe_println(&format!("Failed to initialize logger: {}", e));
                // We continue without initializing the logger
            }
        }
        
        // DO NOT try to log anything here - it can cause circular dependencies
        // since the logger might not be fully initialized yet
    });
}

// Internal function to log a message
fn log_message(level: LogLevel, message: &str, source: &str, details: Option<&str>) -> Result<(), String> {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
    
    let entry = LogEntry {
        timestamp,
        level: level.clone(),
        message: message.to_string(),
        source: source.to_string(),
        details: details.map(|s| s.to_string()),
    };
    
    // Send error and critical messages to Sentry asynchronously
    if matches!(level, LogLevel::Error | LogLevel::Critical) {
        // Clone values for the thread
        let message_clone = message.to_string();
        let source_clone = source.to_string();
        let details_clone = details.map(|s| s.to_string());
        let level_clone = level.clone();
        
        // Spawn a thread to handle Sentry reporting
        thread::spawn(move || {
            // Convert LogLevel to Sentry Level
            let sentry_level: sentry::Level = level_clone.into();
            
            // Create a Sentry event
            let mut event = sentry::protocol::Event {
                message: Some(message_clone),
                level: sentry_level,
                ..Default::default()
            };
            
            // Add context data
            let mut extra = std::collections::BTreeMap::new();
            if let Some(details_str) = details_clone {
                extra.insert("details".to_string(), JsonValue::String(details_str));
            }
            extra.insert("source".to_string(), JsonValue::String(source_clone));
            event.extra = extra;
            
            // Capture the event in Sentry
            sentry::capture_event(event);
        });
    }
    
    // Try to get the logger
    let logger_guard = LOGGER.lock().map_err(|e| format!("Failed to lock logger: {}", e))?;
    
    if let Some(logger) = &*logger_guard {
        logger.log(&entry)
    } else {
        Err("Logger not initialized".to_string())
    }
}

// Tauri command to log a message from JS
#[command]
pub fn log_from_js(level: LogLevel, message: String, source: String, details: Option<String>) -> Result<(), String> {
    log_message(level, &message, &source, details.as_deref())
}

// Convenience functions for Rust-side logging
pub fn debug(message: &str, source: &str, details: Option<&str>) -> Result<(), String> {
    log_message(LogLevel::Debug, message, source, details)
}

pub fn info(message: &str, source: &str, details: Option<&str>) -> Result<(), String> {
    log_message(LogLevel::Info, message, source, details)
}

pub fn warning(message: &str, source: &str, details: Option<&str>) -> Result<(), String> {
    log_message(LogLevel::Warning, message, source, details)
}

pub fn error(message: &str, source: &str, details: Option<&str>) -> Result<(), String> {
    // Add breadcrumb for Sentry
    sentry::add_breadcrumb(sentry::Breadcrumb {
        category: Some("error".to_string()),
        message: Some(message.to_string()),
        data: {
            let mut map = std::collections::BTreeMap::new();
            map.insert("source".to_string(), JsonValue::String(source.to_string()));
            if let Some(detail) = details {
                map.insert("details".to_string(), JsonValue::String(detail.to_string()));
            }
            map
        },
        level: sentry::Level::Error,
        ..Default::default()
    });
    
    log_message(LogLevel::Error, message, source, details)
}

pub fn critical(message: &str, source: &str, details: Option<&str>) -> Result<(), String> {
    // Add breadcrumb for Sentry
    sentry::add_breadcrumb(sentry::Breadcrumb {
        category: Some("critical".to_string()),
        message: Some(message.to_string()),
        data: {
            let mut map = std::collections::BTreeMap::new();
            map.insert("source".to_string(), JsonValue::String(source.to_string()));
            if let Some(detail) = details {
                map.insert("details".to_string(), JsonValue::String(detail.to_string()));
            }
            map
        },
        level: sentry::Level::Fatal,
        ..Default::default()
    });
    
    log_message(LogLevel::Critical, message, source, details)
}

// Enable or disable logging
#[command]
pub fn set_logging_enabled(enabled: bool) -> Result<(), String> {
    let mut logger_guard = LOGGER.lock().map_err(|e| format!("Failed to lock logger: {}", e))?;
    
    if let Some(logger) = &mut *logger_guard {
        logger.enabled = enabled;
        Ok(())
    } else {
        Err("Logger not initialized".to_string())
    }
}

// Get the current log file path
#[command]
pub fn get_log_file_path() -> Result<String, String> {
    let logger_guard = LOGGER.lock().map_err(|e| format!("Failed to lock logger: {}", e))?;
    
    if let Some(logger) = &*logger_guard {
        // Ensure the parent directory exists
        if let Some(parent) = logger.log_file_path.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return Err(format!("Failed to create log directory: {}", e));
                }
            }
        }
        
        Ok(logger.log_file_path.to_string_lossy().into_owned())
    } else {
        Err("Logger not initialized".to_string())
    }
}

// Command to retrieve the most recent log entries
#[command]
pub fn get_recent_logs(count: usize) -> Result<Vec<LogEntry>, String> {
    let logger_guard = LOGGER.lock().map_err(|e| format!("Failed to lock logger: {}", e))?;
    
    if let Some(logger) = &*logger_guard {
        // Ensure the parent directory exists
        if let Some(parent) = logger.log_file_path.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return Err(format!("Failed to create log directory: {}", e));
                }
            }
        }
        
        // Check if the log file exists
        if !logger.log_file_path.exists() {
            // Return an empty vector if the file doesn't exist yet
            return Ok(Vec::new());
        }
        
        // Read the log file
        let content = match std::fs::read_to_string(&logger.log_file_path) {
            Ok(content) => content,
            Err(e) => return Err(format!("Failed to read log file: {}", e))
        };
        
        // Parse the log entries (simplified implementation)
        // In a real-world scenario, you might want to use a more robust approach
        let lines: Vec<&str> = content.lines().collect();
        let mut entries = Vec::new();
        
        for line in lines.iter().rev().take(count) {
            // This is a very simple parser and might need improvement
            if let Some(timestamp_end) = line.find("] [") {
                let timestamp = line[1..timestamp_end].to_string();
                
                if let Some(level_end) = line[timestamp_end+3..].find("] [") {
                    let level_str = &line[timestamp_end+3..timestamp_end+3+level_end];
                    let level = match level_str {
                        "DEBUG" => LogLevel::Debug,
                        "INFO" => LogLevel::Info,
                        "WARNING" => LogLevel::Warning,
                        "ERROR" => LogLevel::Error,
                        "CRITICAL" => LogLevel::Critical,
                        _ => LogLevel::Info, // Default
                    };
                    
                    let rest = &line[timestamp_end+3+level_end+3..];
                    if let Some(source_end) = rest.find("] ") {
                        let source = rest[0..source_end].to_string();
                        let message = rest[source_end+2..].to_string();
                        
                        // Split message and details if they exist
                        let (message, details) = if let Some(details_index) = message.find(": ") {
                            (
                                message[0..details_index].to_string(),
                                Some(message[details_index+2..].to_string())
                            )
                        } else {
                            (message, None)
                        };
                        
                        entries.push(LogEntry {
                            timestamp,
                            level,
                            message,
                            source,
                            details,
                        });
                    }
                }
            }
        }
        
        Ok(entries)
    } else {
        Err("Logger not initialized".to_string())
    }
}

// Function to explicitly capture an exception and send it to Sentry
pub fn capture_exception(message: &str, err: &dyn std::error::Error, source: &str, details: Option<&str>) -> Result<(), String> {
    // Create context for the exception
    let mut contexts = std::collections::BTreeMap::new();
    
    // Create error info as a string map context
    let mut error_map = std::collections::BTreeMap::new();
    error_map.insert("type".to_string(), JsonValue::String(err.to_string()));
    if let Some(details_str) = details {
        error_map.insert("details".to_string(), JsonValue::String(details_str.to_string()));
    }
    error_map.insert("source".to_string(), JsonValue::String(source.to_string()));
    
    // Create a proper context object
    let error_context = sentry::protocol::Context::Other(error_map);
    contexts.insert("error_context".to_string(), error_context);
    
    // Capture the exception in Sentry
    sentry::capture_event(sentry::protocol::Event {
        message: Some(message.to_string()),
        level: sentry::Level::Error,
        contexts,
        exception: vec![sentry::protocol::Exception {
            ty: err.to_string(),
            value: Some(message.to_string()),
            ..Default::default()
        }].into(),
        ..Default::default()
    });
    
    // Also log it in our local logger
    error(message, source, details)
}

// Tauri command to capture JavaScript exceptions
#[command]
pub fn capture_exception_from_js(message: String, stack: String, source: String, details: String) -> Result<(), String> {
    // Create a simple error struct to represent JS error
    struct JsError {
        message: String,
        stack: String,
    }
    
    impl std::fmt::Display for JsError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{} (stack: {})", self.message, self.stack)
        }
    }
    
    impl std::fmt::Debug for JsError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "JsError {{ message: {}, stack: {} }}", self.message, self.stack)
        }
    }
    
    impl std::error::Error for JsError {}
    
    // Create JS error instance
    let js_error = JsError {
        message: message.clone(),
        stack,
    };
    
    // Add breadcrumb for Sentry
    sentry::add_breadcrumb(sentry::Breadcrumb {
        category: Some("frontend_error".to_string()),
        message: Some(message.clone()),
        data: {
            let mut map = std::collections::BTreeMap::new();
            map.insert("source".to_string(), JsonValue::String(source.clone()));
            map.insert("details".to_string(), JsonValue::String(details.clone()));
            map
        },
        level: sentry::Level::Error,
        ..Default::default()
    });
    
    // Use the existing capture_exception function
    let details_option = if details.is_empty() { None } else { Some(details.as_str()) };
    capture_exception(&message, &js_error, &source, details_option)
}

// Batch logging from JS
#[command]
pub fn batch_log_from_js(entries: Vec<BatchLogEntry>) -> Result<(), String> {
    for entry in entries {
        log_message(entry.level, &entry.message, &entry.source, entry.details.as_deref())?;
    }
    Ok(())
}

// Structure for batch logging
#[derive(serde::Deserialize)]
pub struct BatchLogEntry {
    level: LogLevel,
    message: String,
    source: String,
    details: Option<String>,
} 