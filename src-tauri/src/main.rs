#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, Semaphore};
use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;
use tauri::{Emitter, Runtime, State, Window, Manager, menu::{Menu, MenuItem, PredefinedMenuItem}};
use tauri::webview::Webview;
use tauri::Url;
use serde::{Deserialize, Serialize};
use reqwest;
use hyper::{Body, Request, Response, Server};
use hyper::upgrade::Upgraded;
use hyper::service::{make_service_fn, service_fn};
use regex::Regex;
use scraper::{Html, Selector, ElementRef};
use anyhow::{Result, anyhow};
use futures::future::try_join_all;
use futures::{SinkExt, StreamExt};
use serde_json;
use sha1::{Sha1, Digest};
use base64::{engine::general_purpose, Engine as _};
use num_cpus;

use tokio_tungstenite::{connect_async, WebSocketStream};
use tokio_tungstenite::tungstenite::protocol::Role as WsRole;
use tokio_tungstenite::tungstenite::{Message as WsMessage, Error as WsError};
use std::pin::Pin;
use native_tls::TlsConnector as NativeTlsConnector;
use url::form_urlencoded;
use human_panic::setup_panic;
use open as open_in_browser;
use notify_rust::Notification;
use flate2::{Decompress, FlushDecompress};

pub mod logger;

// Track if tray minimize hint has been shown this session
static TRAY_HINT_SHOWN: AtomicBool = AtomicBool::new(false);

const SENTRY_DSN: &str = "https://091b4c240c607917457523ff573d65f8@o920931.ingest.us.sentry.io/4508964343054341";
const ENABLE_SENTRY: bool = true;

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ProxyInfo {
    ip: String,
    port: u16,
    country: String,
    city: String,
    isp: String,
    ping_ms: i32,
    proxy_type: String,
    last_checked: String,
    is_working: bool,
    status: String,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct PaginatedProxies {
    proxies: Vec<ProxyInfo>,
    total_count: usize,
    total_pages: usize,
    current_page: usize,
}

#[derive(Default)]
struct AppState {
    proxies: Mutex<Vec<ProxyInfo>>,
    active_proxy: Mutex<Option<ProxyInfo>>,
    is_proxy_running: Mutex<bool>,
    proxy_testing_queue: Mutex<HashSet<String>>,
    fetching_in_progress: Mutex<bool>,
    pipeline_fetching_in_progress: Mutex<bool>,
    pipeline_parsing_in_progress: Mutex<bool>,
    pipeline_testing_in_progress: Mutex<bool>,
    current_proxy_port: Mutex<Option<u16>>,
    fetched_bodies: Mutex<Vec<FetchedBody>>,
    // App handle to emit events from background tasks (e.g., websocket bridge)
    app_handle: StdMutex<Option<tauri::AppHandle>>,
    // Unread notifications/messages count to reflect on badge
    unread_count: Mutex<u32>,
}

#[derive(Clone, Debug)]
struct FetchedBody {
    url: String,
    body: String,
    kind: String,
    meta: String,
}

const SCRAPE_SOURCES: &[(&str, &str)] = &[
    ("https://free-proxy-list.net/", "#proxylisttable > tbody > tr > td:nth-child(1)"),
    ("https://www.sslproxies.org/", "#proxylisttable > tbody > tr > td:nth-child(1)"),
    ("https://www.us-proxy.org/", "#proxylisttable > tbody > tr > td:nth-child(1)"),
    ("https://free-proxy-list.net/uk-proxy.html", "#proxylisttable > tbody > tr > td:nth-child(1)"),
    ("https://free-proxy-list.net/anonymous-proxy.html", "#proxylisttable > tbody > tr > td:nth-child(1)"),
    // Proxynova
    ("https://www.proxynova.com/proxy-server-list/", "table > tbody > tr > td:nth-child(1)"),
    // Geonode
    ("https://geonode.com/free-proxy-list/", "table > tbody > tr > td:nth-child(1)"),
    // Spys.one
    ("https://spys.one/en/", "table.spy1x > tbody > tr > td:nth-child(1)"),
];

// Raw text/API proxy lists
const RAW_SOURCES: &[(&str, &str)] = &[
    // GitHub lists
    ("https://raw.githubusercontent.com/TheSpeedX/SOCKS-List/master/http.txt", "http"),
    ("https://raw.githubusercontent.com/TheSpeedX/SOCKS-List/master/socks4.txt", "socks4"),
    ("https://raw.githubusercontent.com/TheSpeedX/SOCKS-List/master/socks5.txt", "socks5"),
    ("https://raw.githubusercontent.com/ShiftyTR/Proxy-List/master/http.txt", "http"),
    ("https://raw.githubusercontent.com/ShiftyTR/Proxy-List/master/https.txt", "https"),
    ("https://raw.githubusercontent.com/ShiftyTR/Proxy-List/master/socks4.txt", "socks4"),
    ("https://raw.githubusercontent.com/ShiftyTR/Proxy-List/master/socks5.txt", "socks5"),
    ("https://raw.githubusercontent.com/hookzof/socks5_list/master/proxy.txt", "socks5"),
    ("https://raw.githubusercontent.com/monosans/proxy-list/main/proxies/http.txt", "http"),
    ("https://raw.githubusercontent.com/monosans/proxy-list/main/proxies/socks4.txt", "socks4"),
    ("https://raw.githubusercontent.com/monosans/proxy-list/main/proxies/socks5.txt", "socks5"),
    // ProxyScrape API
    ("https://api.proxyscrape.com/v2/?request=getproxies&protocol=http&timeout=10000&country=all&ssl=all&anonymity=all", "http"),
    ("https://api.proxyscrape.com/v2/?request=getproxies&protocol=socks4&timeout=10000&country=all", "socks4"),
    ("https://api.proxyscrape.com/v2/?request=getproxies&protocol=socks5&timeout=10000&country=all", "socks5"),
    // Other APIs (deduped list; removed duplicate of ProxyScrape HTTP)
    ("https://www.proxy-list.download/api/v1/get?type=http", "http"),
    ("https://www.proxy-list.download/api/v1/get?type=https", "https"),
    ("https://www.proxy-list.download/api/v1/get?type=socks4", "socks4"),
    ("https://www.proxy-list.download/api/v1/get?type=socks5", "socks5"),
];

fn is_development_mode() -> bool {
    if let Ok(env_mode) = std::env::var("VITE_NODE_ENV") {
        return env_mode == "development";
    }
    
    cfg!(debug_assertions)
}
// Build a sane X-Super-Properties for login aligned with discord.com origin/referrer
fn build_login_super_properties(user_agent: Option<&str>) -> String {
    let ua = user_agent.unwrap_or(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36"
    );
    // Use stable channel and a generic build number; Discord tolerates slight differences
    let props = serde_json::json!({
        "os": "Windows",
        "browser": "Chrome",
        "device": "",
        "system_locale": "en-US",
        "browser_user_agent": ua,
        "browser_version": "140.0.0.0",
        "os_version": "10",
        "referrer": "https://discord.com/",
        "referring_domain": "discord.com",
        "referrer_current": "https://discord.com/login",
        "referring_domain_current": "discord.com",
        "release_channel": "stable",
        "client_build_number": 446012,
        "client_event_source": serde_json::Value::Null
    });
    let json = props.to_string();
    general_purpose::STANDARD.encode(json.as_bytes())
}

// Resolve application data directory
fn get_app_data_dir() -> PathBuf {
    // Prefer the OS data directory; fall back to a local folder
    let app_data = match dirs::data_dir() {
        Some(dir) => dir.join("com.free-discord.app"),
        None => {
            // Fallback to the executable's directory (logger may not be ready yet)
            eprintln!("Could not determine app data directory, falling back to current directory");
            match std::env::current_dir() {
                Ok(dir) => dir.join("app_data"),
                Err(_) => {
                    // Logger may not be initialized yet
                    eprintln!("Failed to get current directory, using '.'");
                    PathBuf::from("./app_data")
                }
            }
        }
    };
    
    // Ensure directory exists
    if !app_data.exists() {
        match std::fs::create_dir_all(&app_data) {
            Ok(_) => {},
            Err(e) => {
                // Logger may not be initialized yet
                eprintln!("Failed to create app data directory at {:?}: {}", app_data, e);
                // Return a fallback directory in the current path that we'll try to create later
                return PathBuf::from("./app_data");
            }
        }
    }
    
    app_data
}

// Helper function to handle log errors consistently
fn handle_log_error(result: Result<(), String>, fallback_message: &str) {
    if let Err(e) = result {
        // Only use eprintln as a last resort when the logger itself fails
        eprintln!("{}: {}", fallback_message, e);
    }
}

// Log to file and emit to the frontend (throttled)
fn emit_log<R: Runtime>(window: Option<&Window<R>>, message: &str) {
    // Log using the proper logger
    handle_log_error(
        logger::info(message, "ProxyFinder", None),
        "Failed to log message"
    );
    
    // If we have a window, emit the event
    if let Some(window) = window {
        // Always emit step/pipeline messages immediately; throttle other noisy logs
        let _ = window.emit("proxy-log", message);
    }
}

// Command to fetch proxies from various sources
#[tauri::command]
async fn fetch_proxies(window: Window, state: State<'_, Arc<AppState>>) -> Result<Vec<ProxyInfo>, String> {
    // Check if fetching is already in progress
    {
        let mut fetching = state.fetching_in_progress.lock().await;
        if *fetching {
            emit_log(Some(&window), "Proxy fetching already in progress");
            return Err("Proxy fetching already in progress".to_string());
        }
        *fetching = true;
    }
    
    // Create a channel for communication between tasks
    let (tx, _rx) = tokio::sync::mpsc::channel::<ProxyInfo>(100);
    let state_clone = state.inner().clone();
    let window_clone = window.clone();
    
    // Emit initial log
    emit_log(Some(&window), "Starting proxy fetching process...");
    
    // Spawn a task to handle fetching
    tokio::spawn(async move {
        // Use a defer-style pattern to ensure the flag is reset even if there's a panic
        struct FlagResetter<'a> {
            state: &'a Arc<AppState>,
            window: Option<Window>,
            done: bool,
        }
        
        impl<'a> FlagResetter<'a> {
            fn new(state: &'a Arc<AppState>, window: Option<Window>) -> Self {
                Self { state, window, done: false }
            }
            
            fn done(&mut self) {
                self.done = true;
            }
        }
        
        impl<'a> Drop for FlagResetter<'a> {
            fn drop(&mut self) {
                if !self.done {
                    // Use a blocking lock to ensure this happens even during panic
                    let runtime = tokio::runtime::Handle::current();
                    let mut fetching = runtime.block_on(self.state.fetching_in_progress.lock());
                    *fetching = false;
                    let msg = "Reset fetching flag through safety mechanism";
                    emit_log(self.window.as_ref(), msg);
                }
            }
        }
        
        let mut flag_resetter = FlagResetter::new(&state_clone, Some(window_clone.clone()));
        
        // Define a mutex-protected vector to collect all fetched proxies
        let fetched_proxies = Arc::new(Mutex::new(Vec::new()));
        
        // ----- MULTI-THREADED RAW TEXT SOURCE FETCHING -----
        emit_log(Some(&window_clone), "Fetching proxies from raw text sources in parallel...");
        
        // Create a vector to hold all the futures
        let mut raw_fetch_handles = Vec::new();
        
        // Spawn a task for each raw source
        for (url, proxy_type) in RAW_SOURCES {
            // Clone the values for the async task
            let url = url.to_string();
            let proxy_type = proxy_type.to_string();
            let window_clone = window_clone.clone();
            
            // Log that we're starting the fetch
            let source_log = format!("Starting fetch from {} ({})", url, proxy_type);
            emit_log(Some(&window_clone), &source_log);
            
            // Spawn a task for this source
            let handle = tokio::spawn(async move {
                let result = fetch_raw_proxies(&url, &proxy_type).await;
                (url, proxy_type, result)
            });
            
            raw_fetch_handles.push(handle);
        }
        
        // Wait for all raw text fetch operations to complete
        for handle in raw_fetch_handles {
            match handle.await {
                Ok((url, proxy_type, result)) => {
                    match result {
                        Ok(proxies) => {
                            let count_log = format!("Found {} proxies from {} ({})", proxies.len(), url, proxy_type);
                            emit_log(Some(&window_clone), &count_log);
                            
                            // Add these proxies under mutex protection
                            let mut all_proxies_guard = fetched_proxies.lock().await;
                            all_proxies_guard.extend(proxies);
                            
                            // Update the app state with the latest proxies
                            update_state_with_proxies(&state_clone, &window_clone, &all_proxies_guard).await;
                        },
                        Err(e) => {
                            let error_log = format!("Error fetching from {} ({}): {}", url, proxy_type, e);
                            emit_log(Some(&window_clone), &error_log);
                        }
                    }
                },
                Err(e) => {
                    let error_log = format!("Task error when fetching: {}", e);
                    emit_log(Some(&window_clone), &error_log);
                }
            }
        }
        
        // Get the fetched proxies from the mutex
        let mut all_proxies = {
            let proxies_guard = fetched_proxies.lock().await;
            proxies_guard.clone()
        };
        
        // Early exit if we got no proxies from raw sources to avoid wasting time
        if all_proxies.is_empty() {
            emit_log(Some(&window_clone), "No proxies found from RAW_SOURCES, checking HTML sources...");
        } else {
            let found_log = format!("Found {} proxies from raw sources", all_proxies.len());
            emit_log(Some(&window_clone), &found_log);
        }
        
        // ----- MULTI-THREADED HTML SOURCE SCRAPING -----
        emit_log(Some(&window_clone), "Fetching proxies from HTML sources in parallel...");
        
        // Create a vector to hold all the futures
        let mut html_fetch_handles = Vec::new();
        
        // Spawn a task for each HTML source
        for (url, selector) in SCRAPE_SOURCES {
            // Clone the values for the async task
            let url = url.to_string();
            let selector = selector.to_string();
            let window_clone = window_clone.clone();
            
            // Log that we're starting the fetch
            let source_log = format!("Starting scraping from {}", url);
            emit_log(Some(&window_clone), &source_log);
            
            // Spawn a task for this source
            let handle = tokio::spawn(async move {
                let result = scrape_proxies(&url, &selector).await;
                (url, result)
            });
            
            html_fetch_handles.push(handle);
        }
        
        // Wait for all HTML scraping operations to complete
        for handle in html_fetch_handles {
            match handle.await {
                Ok((url, result)) => {
                    match result {
                        Ok(proxies) => {
                            let count_log = format!("Found {} proxies from {}", proxies.len(), url);
                            emit_log(Some(&window_clone), &count_log);
                            
                            // Add these proxies to our master list
                            let all_proxies = proxies.clone();
                            
                            // Update state with all current proxies
                            update_state_with_proxies(&state_clone, &window_clone, &all_proxies).await;
                        },
                        Err(e) => {
                            let error_log = format!("Error scraping from {}: {}", url, e);
                            emit_log(Some(&window_clone), &error_log);
                        }
                    }
                },
                Err(e) => {
                    let error_log = format!("Task error when scraping: {}", e);
                    emit_log(Some(&window_clone), &error_log);
                }
            }
        }
        
        // Early exit if we still have no proxies
        if all_proxies.is_empty() {
            emit_log(Some(&window_clone), "No proxies found from any sources!");
            let mut fetching = state_clone.fetching_in_progress.lock().await;
            *fetching = false;
            flag_resetter.done();
            return;
        }
        
        let found_log = format!("Found {} raw proxies, deduplicating...", all_proxies.len());
        emit_log(Some(&window_clone), &found_log);
    
        // Deduplicate proxies
        let mut seen = HashSet::new();
        all_proxies.retain(|proxy| seen.insert(format!("{}:{}", proxy.ip, proxy.port)));
        
        let dedup_log = format!("After deduplication: {} unique proxies", all_proxies.len());
        emit_log(Some(&window_clone), &dedup_log);
        
        // Store the proxies in state immediately
        {
            let mut proxies_lock = state_clone.proxies.lock().await;
            *proxies_lock = all_proxies.clone();
            
            // Emit an event to notify the frontend of new proxies
            let _ = window_clone.emit("proxies-updated", true);
        }
        
        // Start testing proxies in parallel with concurrency limit
        let test_log = format!("Starting to test {} proxies in parallel...", all_proxies.len());
        emit_log(Some(&window_clone), &test_log);
        
    let semaphore = Arc::new(Semaphore::new(30)); // Limit concurrent tests
    // Throttle UI update events to avoid spamming the WebView (PostMessage queue overflow)
    let last_emit_time = Arc::new(Mutex::new(Instant::now()));
        let mut handles = vec![];
        
        for proxy in all_proxies {
            let tx = tx.clone();
            let state_clone = state_clone.clone();
            let semaphore = semaphore.clone();
            let ip = proxy.ip.clone();
            let port = proxy.port;
            let window_clone = window_clone.clone();
            
            // Track which proxy is being tested
            {
                let mut queue = state_clone.proxy_testing_queue.lock().await;
                queue.insert(format!("{}:{}", ip, port));
            }
            
            // Emit a progress event for the frontend
            let queue_size = {
                let queue = state_clone.proxy_testing_queue.lock().await;
                queue.len()
            };
            let progress_log = format!("Testing proxy: {}:{} (Queue size: {})", ip, port, queue_size);
            emit_log(Some(&window_clone), &progress_log);
            
            // Clone throttling handle for this iteration/closure
            let last_emit_time_cloned = last_emit_time.clone();
            // Emit an event to update the UI with testing status (throttled)
            {
                let mut last = last_emit_time_cloned.lock().await;
                if last.elapsed() >= Duration::from_millis(300) {
                    let _ = window_clone.emit("proxy-testing-status", (queue_size, true));
                    *last = Instant::now();
                }
            }
            
            // Spawn a new task for each proxy test
            let handle = tokio::spawn(async move {
                // Use a timeout for the semaphore acquisition to avoid deadlocks
                let permit = match timeout(Duration::from_secs(30), semaphore.acquire()).await {
                    Ok(Ok(permit)) => permit,
                    Ok(Err(e)) => {
                        let error_log = format!("Error acquiring semaphore for {}:{}: {}", ip, port, e);
                        emit_log(Some(&window_clone), &error_log);
                        // Remove from testing queue
                        let mut queue = state_clone.proxy_testing_queue.lock().await;
                        queue.remove(&format!("{}:{}", ip, port));
                        
                        // Emit an event to update the UI with testing status
                        let queue_size = queue.len();
                        let _ = window_clone.emit("proxy-testing-status", (queue_size, true));
                        return;
                    },
                    Err(_) => {
                        let timeout_log = format!("Timeout acquiring semaphore for {}:{}", ip, port);
                        emit_log(Some(&window_clone), &timeout_log);
                        // Remove from testing queue
                        let mut queue = state_clone.proxy_testing_queue.lock().await;
                        queue.remove(&format!("{}:{}", ip, port));
                        
                        // Emit an event to update the UI with testing status
                        let queue_size = queue.len();
                        let _ = window_clone.emit("proxy-testing-status", (queue_size, true));
                        return;
                    }
                };
                
                // Test the proxy
                match test_proxy_internal(&ip, port, &state_clone).await {
                    Ok(tested_proxy) => {
                        // Send the tested proxy through the channel
                        let _ = tx.send(tested_proxy.clone()).await;
                        
                        // Log the result
                        let result_log = if tested_proxy.is_working {
                            format!("Proxy {}:{} is working! Ping: {}ms", ip, port, tested_proxy.ping_ms)
                        } else {
                            format!("Proxy {}:{} is not working", ip, port)
                        };
                        emit_log(Some(&window_clone), &result_log);
                        
                        // Throttled UI update to prevent excessive events per tested proxy
                        {
                            let mut last = last_emit_time_cloned.lock().await;
                            if last.elapsed() >= Duration::from_millis(300) {
                                let _ = window_clone.emit("proxies-updated", true);
                                *last = Instant::now();
                            }
                        }
                    },
                    Err(e) => {
                        let error_log = format!("Error testing proxy {}:{}: {}", ip, port, e);
                        emit_log(Some(&window_clone), &error_log);
                    }
                }
                
                // Remove from testing queue
                let mut queue = state_clone.proxy_testing_queue.lock().await;
                queue.remove(&format!("{}:{}", ip, port));
                
                // Emit an event to update the UI with testing status (throttled)
                let queue_size = queue.len();
                {
                    let mut last = last_emit_time_cloned.lock().await;
                    if last.elapsed() >= Duration::from_millis(300) {
                        let _ = window_clone.emit("proxy-testing-status", (queue_size, true));
                        *last = Instant::now();
                    }
                }
                
                // Drop the permit explicitly
                drop(permit);
            });
            
            handles.push(handle);
        }
        
        // Drop the sender to signal no more messages will come
        drop(tx);
        
        let waiting_log = format!("Waiting for {} proxy tests to complete...", handles.len());
        emit_log(Some(&window_clone), &waiting_log);
        
        // Wait for all tests to complete with a timeout
        let results = timeout(Duration::from_secs(300), try_join_all(handles)).await;
        match results {
            Ok(_) => emit_log(Some(&window_clone), "All proxy tests completed!"),
            Err(_) => emit_log(Some(&window_clone), "Some proxy tests timed out after 5 minutes"),
        }
        
        // Reset the fetching flag
        let mut fetching = state_clone.fetching_in_progress.lock().await;
        *fetching = false;
        flag_resetter.done();
        
        emit_log(Some(&window_clone), "Completed proxy fetching process");
        
        // Emit final status
        let proxies_count = {
            let proxies = state_clone.proxies.lock().await;
            proxies.len()
        };
        let working_count = {
            let proxies = state_clone.proxies.lock().await;
            proxies.iter().filter(|p| p.is_working).count()
        };
        let final_log = format!("Found {} proxies, {} working", proxies_count, working_count);
        emit_log(Some(&window_clone), &final_log);
        
        // Final UI update
        let _ = window_clone.emit("proxies-updated", true);
        let _ = window_clone.emit("proxy-testing-status", (0, false));
    });
    
    // Return the current list of proxies immediately
    let proxies = state.proxies.lock().await;
    Ok(proxies.clone())
}

// -------- Pipeline (Step-based) --------
#[tauri::command]
async fn pipeline_reset(window: Window, state: State<'_, Arc<AppState>>) -> Result<String, String> {
    // Disallow reset while any pipeline step is running
    {
        let fetching = state.pipeline_fetching_in_progress.lock().await;
        let parsing = state.pipeline_parsing_in_progress.lock().await;
        let testing = state.pipeline_testing_in_progress.lock().await;
        if *fetching || *parsing || *testing {
            emit_log(Some(&window), "[Pipeline] Reset requested while pipeline is busy; ignoring");
            return Err("Pipeline busy".into());
        }
    }
    {
        let mut b = state.fetched_bodies.lock().await;
        let n = b.len();
        b.clear();
        emit_log(Some(&window), &format!("Pipeline reset: cleared {} fetched bodies", n));
    }
    Ok("ok".into())
}

#[tauri::command]
async fn pipeline_fetch_sources(window: Window, state: State<'_, Arc<AppState>>) -> Result<String, String> {
    // Block if other steps are running
    {
        let parsing = state.pipeline_parsing_in_progress.lock().await;
        let testing = state.pipeline_testing_in_progress.lock().await;
        if *parsing || *testing {
            emit_log(Some(&window), "[Step 1] Cannot start: another step in progress");
            return Err("Pipeline busy".into());
        }
    }
    // Guard against concurrent runs
    {
        let mut flag = state.pipeline_fetching_in_progress.lock().await;
        if *flag {
            emit_log(Some(&window), "[Step 1] Fetch already in progress; ignoring duplicate call");
            return Ok("already_running".into());
        }
        *flag = true;
    }
    emit_log(Some(&window), "[Step 1] Fetching sources (raw + html)…");
    {
        let mut b = state.fetched_bodies.lock().await;
        b.clear();
    }

    let base_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .connect_timeout(Duration::from_secs(5))
        .tcp_nodelay(true)
        .build()
        .map_err(|e| e.to_string())?;

    let semaphore = Arc::new(Semaphore::new(12));
    let mut handles = Vec::new();

    // RAW
    for (url, ptype) in RAW_SOURCES {
        let url = url.to_string();
        let ptype = ptype.to_string();
        let w = window.clone();
        let sem = semaphore.clone();
    let client = base_client.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.ok();
            emit_log(Some(&w), &format!("Fetching RAW {}", url));
            match tokio::time::timeout(Duration::from_secs(12), client.get(url.clone()).header("User-Agent", "Mozilla/5.0").send()).await {
                Ok(Ok(resp)) => match resp.text().await {
                    Ok(body) if !body.is_empty() => Some(FetchedBody { url, body, kind: "raw".into(), meta: ptype }),
                    _ => None,
                },
                _ => None,
            }
        }));
    }

    // HTML
    for (url, selector) in SCRAPE_SOURCES {
        let url = url.to_string();
        let selector = selector.to_string();
        let w = window.clone();
        let sem = semaphore.clone();
    let client = base_client.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.ok();
            emit_log(Some(&w), &format!("Fetching HTML {}", url));
            match tokio::time::timeout(Duration::from_secs(12), client.get(url.clone()).header("User-Agent", "Mozilla/5.0").send()).await {
                Ok(Ok(resp)) => match resp.text().await {
                    Ok(body) if !body.is_empty() => Some(FetchedBody { url, body, kind: "html".into(), meta: selector }),
                    _ => None,
                },
                _ => None,
            }
        }));
    }

    let wait = tokio::time::timeout(Duration::from_secs(40), try_join_all(handles)).await;
    let mut collected: Vec<FetchedBody> = Vec::new();
    if let Ok(Ok(results)) = wait {
        for item in results.into_iter().flatten() {
            collected.push(item);
        }
    } else {
        emit_log(Some(&window), "Some sources timed out; using partial results");
    }

    let count = collected.len();
    {
        let mut b = state.fetched_bodies.lock().await;
        *b = collected;
    }
    emit_log(Some(&window), &format!("Fetched {} bodies", count));
    // Reset in-progress flag
    {
        let mut flag = state.pipeline_fetching_in_progress.lock().await;
        *flag = false;
    }
    Ok(format!("fetched_bodies:{}", count))
}

#[tauri::command]
async fn pipeline_parse_fetched(window: Window, state: State<'_, Arc<AppState>>) -> Result<usize, String> {
    // Block if fetch or test running
    {
        let fetching = state.pipeline_fetching_in_progress.lock().await;
        let testing = state.pipeline_testing_in_progress.lock().await;
        if *fetching || *testing {
            emit_log(Some(&window), "[Step 2] Cannot start: another step in progress");
            return Err("Pipeline busy".into());
        }
    }
    // Guard concurrent parse
    {
        let mut flag = state.pipeline_parsing_in_progress.lock().await;
        if *flag {
            emit_log(Some(&window), "[Step 2] Parse already in progress; ignoring duplicate call");
            return Ok({ let lock = state.proxies.lock().await; lock.len() });
        }
        *flag = true;
    }
    emit_log(Some(&window), "[Step 2] Parsing fetched bodies…");
    let bodies = state.fetched_bodies.lock().await.clone();
    let total_bodies = bodies.len().max(1);
    let mut processed_bodies: usize = 0;
    let mut candidates_total: usize = 0;
    // Output will be collected in shared Vec via out_arc
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    // progress logging handled after parallel section
    // Emit an initial progress update
    emit_log(Some(&window), &format!(
        "[Step 2] Parsing… {}/{} bodies ({}%), candidates so far: {}",
        processed_bodies, total_bodies, 0, candidates_total
    ));

    // Process bodies in parallel to speed up heavy pages
    let bodies_arc = Arc::new(bodies);
    let out_arc = Arc::new(Mutex::new(Vec::with_capacity(16384)));
    let cand_arc = Arc::new(Mutex::new(0usize));
    let mut tasks = Vec::new();
    let max_tasks = 12usize;
    let bodies_slice: &[_] = &*bodies_arc; // &[FetchedBody]
    let chunk_size = std::cmp::max(1, (bodies_slice.len() + max_tasks - 1) / max_tasks);
    for chunk in bodies_slice.chunks(chunk_size) {
        let chunk = chunk.to_vec();
        let out_arc = out_arc.clone();
        let cand_arc = cand_arc.clone();
        let now_s = now.clone();
        tasks.push(tokio::spawn(async move {
            // Fast regexes
            let ip_re = Regex::new(r"(\d{1,3}\.){3}\d{1,3}").ok();
            let port_re = Regex::new(r">(\d{2,5})<").ok();
            let mut local: Vec<ProxyInfo> = Vec::new();
            let mut local_count = 0usize;
            for fb in chunk.into_iter() {
                if fb.kind == "raw" {
                    for line in fb.body.lines() {
                        let line = line.trim();
                        if line.is_empty() || line.starts_with('#') { continue; }
                        let parts: Vec<&str> = line.split(':').collect();
                        let (ip, port) = match parts.len() { 1 => (parts[0].trim(), 80u16), 2 => { if let Ok(p) = parts[1].trim().parse::<u16>() { (parts[0].trim(), p) } else { continue } }, _ => continue };
                        if !is_valid_ip(ip) { continue; }
                        local.push(ProxyInfo { ip: ip.to_string(), port, country: "Unknown".into(), city: "Unknown".into(), isp: "Unknown".into(), ping_ms: -1, proxy_type: fb.meta.clone(), last_checked: now_s.clone(), is_working: false, status: "pending".into(), username: None, password: None });
                        local_count += 1;
                    }
                } else if fb.kind == "html" {
                    // Try fast path: regex for IPs and corresponding port column
                    if let (Some(ip_re), Some(port_re)) = (ip_re.as_ref(), port_re.as_ref()) {
                        // Rough heuristic: for each table row, try to pick first IP and second column as port
                        for row in fb.body.split("</tr>") {
                            if let Some(ip_caps) = ip_re.find(row) {
                                let ip = row[ip_caps.start()..ip_caps.end()].to_string();
                                if is_valid_ip(&ip) {
                                    // Find second column port
                                    let mut port_val: u16 = 0;
                                    // pick last numeric in row as port candidate
                                    for cap in port_re.captures_iter(row) {
                                        if let Some(m) = cap.get(1) {
                                            if let Ok(p) = m.as_str().trim().parse::<u16>() { port_val = p; }
                                        }
                                    }
                                    if port_val > 0 {
                                        let ptype = if fb.url.contains("socks4") { "socks4" } else if fb.url.contains("socks5") { "socks5" } else if fb.url.contains("https") { "https" } else { "http" };
                                        local.push(ProxyInfo { ip, port: port_val, country: "Unknown".into(), city: "Unknown".into(), isp: "Unknown".into(), ping_ms: -1, proxy_type: ptype.into(), last_checked: now_s.clone(), is_working: false, status: "pending".into(), username: None, password: None });
                                        local_count += 1;
                                    }
                                }
                            }
                        }
                    } else {
                        // Fallback: use scraper selector
                        if let Ok(sel) = Selector::parse(&fb.meta) {
                            let doc = Html::parse_document(&fb.body);
                            for el in doc.select(&sel) {
                                let ip = el.inner_html().trim().to_string();
                                if ip.is_empty() || !is_valid_ip(&ip) { continue; }
                                let port = if let Some(parent) = el.parent() { if let Some(pel) = ElementRef::wrap(parent) { if let Ok(s2) = Selector::parse("td:nth-child(2)") { pel.select(&s2).next().and_then(|e| e.inner_html().trim().parse::<u16>().ok()).unwrap_or(0) } else { 0 } } else { 0 } } else { 0 };
                                if port == 0 { continue; }
                                let ptype = if fb.url.contains("socks4") { "socks4" } else if fb.url.contains("socks5") { "socks5" } else if fb.url.contains("https") { "https" } else { "http" };
                                local.push(ProxyInfo { ip, port, country: "Unknown".into(), city: "Unknown".into(), isp: "Unknown".into(), ping_ms: -1, proxy_type: ptype.into(), last_checked: now_s.clone(), is_working: false, status: "pending".into(), username: None, password: None });
                                local_count += 1;
                            }
                        }
                    }
                }
            }
            // Merge into shared output
            if local_count > 0 {
                let mut out_g = out_arc.lock().await;
                out_g.extend(local);
                let mut c_g = cand_arc.lock().await;
                *c_g += local_count;
            }
        }));
    }
    // Join tasks with timeout
    let _ = tokio::time::timeout(Duration::from_secs(90), try_join_all(tasks)).await;
    // Read shared results
    {
        let cand = cand_arc.lock().await;
        candidates_total = *cand;
    }
    let mut out: Vec<ProxyInfo> = out_arc.lock().await.clone();
    processed_bodies = total_bodies; // completed

    // Final progress update to show completion of parsing stage before dedup
    emit_log(Some(&window), &format!(
        "[Step 2] Parsing… {}/{} bodies (100%), candidates so far: {}",
        processed_bodies, total_bodies, candidates_total
    ));

    // dedup
    emit_log(Some(&window), &format!("[Step 2] Deduplicating {} candidates…", out.len()));
    let mut seen = HashSet::new();
    out.retain(|p| seen.insert(format!("{}:{}", p.ip, p.port)));
    let total = out.len();
    {
        let mut lock = state.proxies.lock().await;
        *lock = out;
    }
    let _ = window.emit("proxies-updated", true);
    emit_log(Some(&window), &format!("[Step 2] Parsed {} unique proxies (from {} bodies)", total, total_bodies));
    // Reset parse in-progress flag
    {
        let mut flag = state.pipeline_parsing_in_progress.lock().await;
        *flag = false;
    }
    Ok(total)
}

#[tauri::command]
async fn pipeline_test_all(window: Window, state: State<'_, Arc<AppState>>) -> Result<String, String> {
    // Block if fetch or parse running
    {
        let fetching = state.pipeline_fetching_in_progress.lock().await;
        let parsing = state.pipeline_parsing_in_progress.lock().await;
        if *fetching || *parsing {
            emit_log(Some(&window), "[Step 3] Cannot start: another step in progress");
            return Err("Pipeline busy".into());
        }
    }
    // Prevent duplicate test runs
    {
        let mut flag = state.pipeline_testing_in_progress.lock().await;
        if *flag {
            emit_log(Some(&window), "[Step 3] Test already running; ignoring duplicate call");
            return Ok("already_running".into());
        }
        *flag = true;
    }
    let current = { state.proxies.lock().await.clone() };
    if current.is_empty() { return Err("No proxies to test".into()); }
    emit_log(Some(&window), &format!("[Step 3] Testing {} proxies…", current.len()));
    let cores = num_cpus::get();
    let mut concurrency = cores.saturating_mul(8);
    if concurrency < 8 { concurrency = 8; }
    if concurrency > 48 { concurrency = 48; }
    let sem = Arc::new(Semaphore::new(concurrency));
    let mut handles = Vec::new();
    let last_emit_time = Arc::new(Mutex::new(Instant::now()));
    for p in current.into_iter() {
        let st = state.inner().clone();
        let w = window.clone();
        let s = sem.clone();
        let ip = p.ip.clone();
        let port = p.port;
        let last_emit_time_cloned = last_emit_time.clone();
        {
            let mut q = st.proxy_testing_queue.lock().await;
            q.insert(format!("{}:{}", ip, port));
        }
        handles.push(tokio::spawn(async move {
            let _permit = s.acquire().await.ok();
            let _ = test_proxy_internal(&ip, port, &st).await;
            {
                let mut q = st.proxy_testing_queue.lock().await;
                q.remove(&format!("{}:{}", ip, port));
                let size = q.len();
                let mut last = last_emit_time_cloned.lock().await;
                if last.elapsed() >= Duration::from_millis(300) {
                    let _ = w.emit("proxy-testing-status", (size, true));
                    let _ = w.emit("proxies-updated", true);
                    *last = Instant::now();
                }
            }
        }));
    }
    // Slightly shorter overall timeout to avoid long stalls
    let _ = tokio::time::timeout(Duration::from_secs(240), try_join_all(handles)).await;
    let _ = window.emit("proxy-testing-status", (0usize, false));
    emit_log(Some(&window), "Testing complete");
    {
        let mut flag = state.pipeline_testing_in_progress.lock().await;
        *flag = false;
    }
    Ok("ok".into())
}

// Helper function to update the application state with new proxies
async fn update_state_with_proxies(state: &Arc<AppState>, window: &Window, new_proxies: &[ProxyInfo]) {
    // Use a HashSet to track unique proxies
    let mut seen = HashSet::new();
    let mut current_proxies = Vec::new();
    
    // Add previously found proxies
    {
        let proxies_lock = state.proxies.lock().await;
        for proxy in proxies_lock.iter() {
            let key = format!("{}:{}", proxy.ip, proxy.port);
            if seen.insert(key) {
                current_proxies.push(proxy.clone());
            }
        }
    }
    
    // Add new proxies
    for proxy in new_proxies {
        let key = format!("{}:{}", proxy.ip, proxy.port);
        if seen.insert(key) {
            current_proxies.push(proxy.clone());
        }
    }
    
    // Update state with combined list
    {
        let mut proxies_lock = state.proxies.lock().await;
        *proxies_lock = current_proxies;
    }
    
    // Emit an event to notify the frontend of new proxies
    let _ = window.emit("proxies-updated", true);
}

// Scrape proxies from an HTML table
async fn scrape_proxies(url: &str, selector_str: &str) -> Result<Vec<ProxyInfo>> {
    logger::info(&format!("Scraping proxies from {}", url), "ProxyFinder", None)
        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
    
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    
    // Add user agent to avoid being blocked
    let response = match client.get(url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36")
        .send()
        .await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    return Err(anyhow!("Failed to fetch {}: HTTP {}", url, resp.status()));
                }
                resp
            },
            Err(e) => {
                return Err(anyhow!("Failed to fetch {}: {}", url, e));
            }
        };
    
    let body = match response.text().await {
        Ok(text) => text,
        Err(e) => {
            return Err(anyhow!("Failed to read response body from {}: {}", url, e));
        }
    };
    
    if body.is_empty() {
        return Err(anyhow!("Empty response body from {}", url));
    }
    
    let document = Html::parse_document(&body);
    let selector = Selector::parse(selector_str).map_err(|e| anyhow!("Selector error: {:?}", e))?;
    
    let mut proxies = Vec::new();
    
    for element in document.select(&selector) {
        let ip = element.inner_html().trim().to_string();
        
        // Skip empty or invalid IPs
        if ip.is_empty() || !is_valid_ip(&ip) {
            continue;
        }
        
        // Find the port (usually in the next column)
        let port = if let Some(parent) = element.parent() {
            if let Some(parent_element) = ElementRef::wrap(parent) {
                let port_selector = match Selector::parse("td:nth-child(2)") {
                    Ok(selector) => selector,
                    Err(_) => {
                        // Skip this element if we can't parse the selector
                        continue;
                    }
                };
                
                if let Some(port_element) = parent_element.select(&port_selector).next() {
                    port_element.inner_html().trim().parse::<u16>().unwrap_or(0)
                } else {
                    0
                }
            } else {
                0
            }
        } else {
            0
        };
        
        if port > 0 {
            let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
            
            // Determine proxy type based on URL
            let proxy_type = if url.contains("socks4") {
                "socks4"
            } else if url.contains("socks5") {
                "socks5"
            } else if url.contains("https") {
                "https"
            } else {
                "http"
            };
            
            proxies.push(ProxyInfo {
                ip,
                port,
                country: "Unknown".to_string(),
                city: "Unknown".to_string(),
                isp: "Unknown".to_string(),
                ping_ms: -1,
                proxy_type: proxy_type.to_string(),
                last_checked: now,
                is_working: false,
                status: "pending".to_string(),
                username: None,
                password: None,
            });
        }
    }
    
    logger::info(&format!("Found {} proxies from {}", proxies.len(), url), "ProxyFinder", None)
        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
    Ok(proxies)
}

// Helper function to validate IP addresses
fn is_valid_ip(ip: &str) -> bool {
    let re = Regex::new(r"^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$").unwrap();
    if !re.is_match(ip) {
        return false;
    }
    
    // Check each octet is in valid range
    let octets: Vec<&str> = ip.split('.').collect();
    for octet in octets {
        // Parsing into u8 guarantees the value is within 0..=255; if parsing fails, it's invalid
        if octet.parse::<u8>().is_err() {
            return false;
        }
    }
    
    true
}

// Helper to extract IPs from script elements (for ProxyNova)
#[allow(dead_code)]
fn extract_ip_from_script(script: &str) -> Option<String> {
    // Example: document.write('123.45' + '.67.89')
    let re = Regex::new(r"document\.write\(.*?'(.*?)'.*?\+.*?'(.*?)'.*?\)").ok()?;
    if let Some(captures) = re.captures(script) {
        let part1 = captures.get(1)?.as_str();
        let part2 = captures.get(2)?.as_str();
        return Some(format!("{}{}", part1, part2));
    }
    
    // Simpler pattern fallback
    let re2 = Regex::new(r"(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3})").ok()?;
    re2.captures(script).map(|caps| caps.get(1).unwrap().as_str().to_string())
}

// Helper to extract IPs from Spys.one HTML
#[allow(dead_code)]
fn extract_ip_from_spys(html: &str) -> Option<String> {
    let re = Regex::new(r"(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3})").ok()?;
    re.captures(html).map(|caps| caps.get(1).unwrap().as_str().to_string())
}

// Function to fetch proxies from raw text files
async fn fetch_raw_proxies(url: &str, proxy_type: &str) -> Result<Vec<ProxyInfo>> {
    logger::info(&format!("Fetching raw proxies from {}", url), "ProxyFinder", None)
        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
    
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    
    let response = match client.get(url)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36")
        .send()
        .await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    return Err(anyhow!("Failed to fetch {}: HTTP {}", url, resp.status()));
                }
                resp
            },
            Err(e) => {
                return Err(anyhow!("Failed to fetch {}: {}", url, e));
            }
        };
    
    let body = match response.text().await {
        Ok(text) => text,
        Err(e) => {
            return Err(anyhow!("Failed to read response body from {}: {}", url, e));
        }
    };
    
    if body.is_empty() {
        return Err(anyhow!("Empty response body from {}", url));
    }
    
    let lines: Vec<&str> = body.lines().collect();
    let mut proxies = Vec::new();
    
    logger::debug(&format!("Processing {} lines from {}", lines.len(), url), "ProxyFinder", None)
        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
    
    for line in lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        
        // Different formats: IP:PORT or just IP (with port elsewhere)
        let parts: Vec<&str> = line.split(':').collect();
        let (ip, port) = match parts.len() {
            1 => (parts[0].trim(), 80), // Default to port 80 if not specified
            2 => {
                let port_str = parts[1].trim();
                let port = match port_str.parse::<u16>() {
                    Ok(p) => p,
                    Err(_) => {
                        logger::warning(&format!("Invalid port in line '{}' from {}", line, url), "ProxyFinder", None)
                            .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
                        continue;
                    }
                };
                (parts[0].trim(), port)
            },
            _ => {
                logger::warning(&format!("Invalid format in line '{}' from {}", line, url), "ProxyFinder", None)
                    .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
                continue;
            }
        };
        
        // Validate IP
        if !is_valid_ip(ip) {
            logger::warning(&format!("Invalid IP '{}' in line '{}' from {}", ip, line, url), "ProxyFinder", None)
                .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
            continue;
        }
        
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
        
        proxies.push(ProxyInfo {
            ip: ip.to_string(),
            port,
            country: "Unknown".to_string(),
            city: "Unknown".to_string(),
            isp: "Unknown".to_string(),
            ping_ms: -1,
            proxy_type: proxy_type.to_string(),
            last_checked: now,
            is_working: false,
            status: "pending".to_string(),
            username: None,
            password: None,
        });
    }
    
    logger::info(&format!("Found {} valid proxies from {}", proxies.len(), url), "ProxyFinder", None)
        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
    Ok(proxies)
}

// Internal function to test a proxy without requiring the Tauri state
async fn test_proxy_internal(
    ip: &str, 
    port: u16,
    state: &Arc<AppState>
) -> Result<ProxyInfo, String> {
    let mut proxy_info = {
        let proxies = state.proxies.lock().await;
        proxies.iter()
            .find(|p| p.ip == ip && p.port == port)
            .cloned()
            .unwrap_or_else(|| {
                let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
                ProxyInfo {
                    ip: ip.to_string(),
                    port,
                    country: "Unknown".to_string(),
                    city: "Unknown".to_string(),
                    isp: "Unknown".to_string(),
                    ping_ms: -1,
                    proxy_type: "http".to_string(),
                    last_checked: now,
                    is_working: false,
                    status: "pending".to_string(),
                    username: None,
                    password: None,
                }
            })
    };
    
    // Update status to testing
    proxy_info.status = "testing".to_string();
    
    // Update proxy in state to show it's being tested
    {
        let mut proxies = state.proxies.lock().await;
        if let Some(index) = proxies.iter().position(|p| p.ip == ip && p.port == port) {
            proxies[index].status = "testing".to_string();
        }
    }
    
    // Check if proxy is working by connecting to it
    let addr = format!("{}:{}", ip, port);
    let socket_addr = match addr.parse::<SocketAddr>() {
        Ok(addr) => addr,
        Err(_) => {
            proxy_info.status = "failed".to_string();
            return Err(format!("Invalid address: {}", addr));
        }
    };
    
    // Use shorter timeout (3 seconds) with fewer attempts for faster testing
    let start = Instant::now();
    let mut connection_attempts = 0;
    let max_attempts = 2;
    let mut is_working = false;
    
    while connection_attempts < max_attempts && !is_working {
        connection_attempts += 1;
        
        match timeout(Duration::from_secs(3), TcpStream::connect(socket_addr)).await {
            Ok(Ok(_)) => {
                is_working = true;
                break;
            },
            Ok(Err(e)) => {
                logger::debug(
                    &format!("Connection attempt {} failed for {}:{}: {}", 
                            connection_attempts, ip, port, e),
                    "ProxyTester", 
                    None
                ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
                
                // Wait a bit before retrying
                if connection_attempts < max_attempts {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
            },
            Err(_) => {
                logger::debug(
                    &format!("Connection timeout attempt {} for {}:{}", 
                            connection_attempts, ip, port),
                    "ProxyTester", 
                    None
                ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
                
                // Wait a bit before retrying
                if connection_attempts < max_attempts {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
            }
        }
    }
    let ping_ms = if is_working {
        start.elapsed().as_millis() as i32
    } else {
        -1
    };
    
    // If basic connection works, test if proxy can reach Discord
    if is_working {
        logger::debug(&format!("Testing Discord connectivity through proxy {}:{}", ip, port), "ProxyTester", None).ok();
        is_working = test_discord_connectivity(ip, port, &proxy_info.proxy_type).await;
        if !is_working {
            logger::debug(&format!("Proxy {}:{} cannot reach Discord", ip, port), "ProxyTester", None).ok();
        }
    }
    
    proxy_info.is_working = is_working;
    proxy_info.ping_ms = ping_ms;
    proxy_info.last_checked = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    proxy_info.status = if is_working { "tested".to_string() } else { "failed".to_string() };
    
    // If working, get geolocation info
    if is_working {
        match get_ip_geolocation(ip).await {
            Ok(geo) => {
                proxy_info.country = geo.country;
                proxy_info.city = geo.city;
                proxy_info.isp = geo.isp;
            },
            Err(e) => {
                logger::warning(
                    &format!("Error getting geolocation for {}:{}", ip, port),
                    "ProxyFinder",
                    Some(&e.to_string())
                ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
            }
        }
        
        // Optionally run an additional HTTP probe in the background without blocking
        let ip_owned = ip.to_string();
        let proxy_type_owned = proxy_info.proxy_type.clone();
        tokio::spawn(async move {
            if let Err(e) = test_http_proxy(&ip_owned, port, &proxy_type_owned).await {
                logger::debug(&format!("Background HTTP probe failed for {}:{}: {}", ip_owned, port, e), "ProxyTester", None).ok();
            }
        });
    }
    
    // Update proxy in state
    let mut proxies = state.proxies.lock().await;
    if let Some(index) = proxies.iter().position(|p| p.ip == ip && p.port == port) {
        proxies[index] = proxy_info.clone();
    } else {
        proxies.push(proxy_info.clone());
    }
    
    // If the proxy is working, save it to cache
    if is_working {
        // Fix the borrowed data escape error by cloning the Arc before spawning
        let state_clone = Arc::clone(state);
        // Fire and forget - don't wait for this to complete
        tokio::spawn(async move {
            if let Err(e) = save_proxies_to_cache(&state_clone).await {
                eprintln!("Failed to save proxies to cache: {}", e);
            }
        });
    }
    
    Ok(proxy_info)
}

// Additional function to test if proxy works for HTTP requests and Discord HTTPS specifically
async fn test_http_proxy(ip: &str, port: u16, proxy_type: &str) -> Result<()> {
    // Create proxy URL based on proxy type and build two independent Proxy instances
    let proxy_url = match proxy_type {
        "socks5" => format!("socks5h://{}:{}", ip, port),
        "socks4" => format!("socks5h://{}:{}", ip, port), // fallback to socks5h
        _ => format!("http://{}:{}", ip, port),
    };

    // Build two separate reqwest::Proxy instances so we don't move the same value twice
    let main_proxy = if proxy_type == "socks5" || proxy_type == "socks4" {
        reqwest::Proxy::all(&proxy_url)?
    } else {
        // Use Proxy::all so HTTPS requests also go through the HTTP proxy
        reqwest::Proxy::all(&proxy_url)?
    };

    let probe_proxy = if proxy_type == "socks5" || proxy_type == "socks4" {
        reqwest::Proxy::all(&proxy_url)?
    } else {
        // Use Proxy::all so HTTPS requests also go through the HTTP proxy
        reqwest::Proxy::all(&proxy_url)?
    };

    // Create a client with the proxy and enhanced timeout settings (strict certs)
    let client = reqwest::Client::builder()
        .proxy(main_proxy)
        .timeout(Duration::from_secs(7))  // shorter overall request timeout
        .connect_timeout(Duration::from_secs(4))  // quicker connect timeout
        .tcp_nodelay(true)
        .danger_accept_invalid_certs(false)  // Ensure proper SSL validation
        .build()?;

    // First test basic connectivity using HTTPS to avoid HTTP-to-HTTPS redirect issues
    // Build a lightweight probe client that allows invalid certs to reduce false negatives
    let probe_client = reqwest::Client::builder()
        .proxy(probe_proxy)
        .timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(3))
        .danger_accept_invalid_certs(true) // Accept invalid certs for probe only
        .tcp_nodelay(true)
        .build()?;

    let mut attempts = 0;
    let max_attempts = 1; // reduce probe attempts

    while attempts < max_attempts {
        attempts += 1;

        match probe_client.get("https://httpbin.org/ip").send().await {
            Ok(response) => {
                if response.status().is_success() {
                    break; // connectivity test passed, continue to Discord HTTPS test
                } else {
                    if attempts < max_attempts {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        continue;
                    }
                    return Err(anyhow!("Proxy returned non-success status for probe HTTPS test: {}", response.status()));
                }
            },
            Err(e) => {
                // Retry on transient network errors
                if attempts < max_attempts && (e.is_timeout() || e.is_connect()) {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
                return Err(anyhow!("Probe HTTPS test failed: {}", e));
            }
        }
    }
    
    // Now test HTTPS connectivity specifically to Discord
    attempts = 0;
    let max_attempts = 2; // fewer retries for speed
    while attempts < max_attempts {
        attempts += 1;

        // Test Discord HTTPS connectivity - use a lightweight endpoint
        match client.get("https://discord.com/api/v9/ping").send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                // Discord ping endpoint may return 404, but successful connection is what matters
                if response.status().is_success() || status == 404 {
                    return Ok(()); // HTTPS to Discord works
                } else if status >= 500 {
                    // Server errors might be temporary, retry
                    logger::warning(
                        &format!("Discord reached but returned server error {} through proxy {}:{} (attempt {})", status, ip, port, attempts),
                        "ProxyTester",
                        None
                    ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));

                    if attempts < max_attempts {
                        tokio::time::sleep(Duration::from_millis(800)).await;
                        continue;
                    }
                    return Err(anyhow!("Proxy cannot reach Discord over HTTPS: server error {}. Try another proxy (prefer SOCKS5).", status));
                } else {
                    // Client errors (400-499) are likely permanent - include status
                    return Err(anyhow!("Proxy cannot reach Discord over HTTPS: client error {}. Try another proxy (prefer SOCKS5).", status));
                }
            },
            Err(e) => {
                // Log the raw error for diagnostics
                logger::warning(
                    &format!("Discord HTTPS test error for proxy {}:{} on attempt {}: {}", ip, port, attempts, e),
                    "ProxyTester",
                    Some(&e.to_string())
                ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));

                if attempts < max_attempts && (e.is_timeout() || e.is_connect() || e.is_request()) {
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    continue;
                }

                return Err(anyhow!("Proxy cannot reach Discord over HTTPS: {}. Try another proxy (prefer SOCKS5).", e));
            }
        }
    }

    Err(anyhow!("Proxy cannot reach Discord over HTTPS after {} attempts. Try another proxy (prefer SOCKS5).", attempts))
}

// Lightweight permissive probe used by start_proxy to quickly validate connectivity
async fn test_http_proxy_probe(ip: &str, port: u16, proxy_type: &str) -> Result<()> {
    // Build proxy URL
    let proxy_url = match proxy_type {
        "socks5" => format!("socks5h://{}:{}", ip, port),
        "socks4" => format!("socks4://{}:{}", ip, port),
        _ => format!("http://{}:{}", ip, port),
    };

    // Build a probe client that accepts invalid certs and uses short timeouts
    let probe_proxy = if proxy_type == "socks5" || proxy_type == "socks4" {
        reqwest::Proxy::all(&proxy_url)?
    } else {
        reqwest::Proxy::http(&proxy_url)?
    };

    let client = reqwest::Client::builder()
        .proxy(probe_proxy)
        .timeout(Duration::from_secs(4))
        .connect_timeout(Duration::from_secs(3))
        .danger_accept_invalid_certs(true)
        .tcp_nodelay(true)
        .build()?;

    // Quick httpbin probe - try HTTP first as fallback for broken HTTPS proxies
    // Do NOT hard fail on non-2xx or transport error; treat as inconclusive and continue to Discord checks
    let probe_result = match client.get("http://httpbin.org/ip").send().await {
        Ok(resp) => Ok(resp),
        Err(e1) => {
            logger::warning(&format!("httpbin HTTP probe failed, trying HTTPS: {}", e1), "ProxyTester", None).ok();
            client.get("https://httpbin.org/ip").send().await
        },
    };

    match probe_result {
        Ok(resp) => {
            if !resp.status().is_success() {
                logger::warning(&format!("httpbin probe returned {} – continuing with Discord-specific checks", resp.status()), "ProxyTester", None).ok();
            }
        }
        Err(e) => {
            logger::warning(&format!("httpbin probe request errored – continuing: {}", e), "ProxyTester", None).ok();
        }
    }

    // Quick Discord HTTPS check (single attempt)
    // If this is an HTTP proxy, try a low-level CONNECT to discord:443 first (faster, avoids TLS errors)
    if proxy_type != "socks5" {
        match probe_http_connect(ip, port, "discord.com").await {
            Ok(()) => return Ok(()),
            Err(e) => {
                logger::debug(&format!("HTTP CONNECT probe failed for {}:{}: {}", ip, port, e), "ProxyTester", None).ok();
                // Fall back to performing the HTTPS request via reqwest client below
            }
        }
    }

    // Try Discord connectivity - be more lenient, try multiple endpoints
    let discord_endpoints = [
        "https://discord.com/api/v9/ping",
        "https://discord.com/api/v9/gateway",
        "http://discord.com/",
        "https://discord.com/",
    ];

    for endpoint in &discord_endpoints {
        match client.get(*endpoint).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                // Treat common non-fatal statuses as connectivity success
                if resp.status().is_success() || [301,302,304,401,403,404,405].contains(&status) {
                    return Ok(());
                }
            }
            Err(_) => continue,
        }
    }

    return Err(anyhow!("All Discord probe attempts failed"));
}

// Ensure the proxy can reach Discord's CDN domains as well (assets/chunks/media)
async fn test_discord_cdn_connectivity(ip: &str, port: u16, proxy_type: &str) -> Result<()> {
    // Build proxy URL
    let proxy_url = match proxy_type {
        "socks5" => format!("socks5h://{}:{}", ip, port),
        "socks4" => format!("socks4://{}:{}", ip, port),
        _ => format!("http://{}:{}", ip, port),
    };

    // Build a client with short, lenient timeouts
    let proxy = if proxy_type == "socks5" || proxy_type == "socks4" {
        reqwest::Proxy::all(&proxy_url)?
    } else {
        // Use Proxy::all so HTTPS CDN requests also go through the HTTP proxy
        reqwest::Proxy::all(&proxy_url)?
    };

    let client = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(3))
        .danger_accept_invalid_certs(true)
        .tcp_nodelay(true)
        .build()?;

    // Try a handful of safe CDN endpoints; success is any HTTP 2xx/3xx/404
    // For HTTP proxies, try http scheme first to avoid TLS issues
    let use_http_scheme = proxy_type.eq_ignore_ascii_case("http") || proxy_type.eq_ignore_ascii_case("https");
    let cdn_endpoints = if use_http_scheme {
        [
            "http://cdn.discord.com/robots.txt",
            "http://cdn.discordapp.com/robots.txt",
            "http://cdn.discord.com/assets/",
            "http://media.discordapp.net/attachments/1/1/1.txt",
        ]
    } else {
        [
            "https://cdn.discord.com/robots.txt",
            "https://cdn.discord.com/assets/",
            "https://cdn.discordapp.com/robots.txt",
            "https://media.discordapp.net/attachments/1/1/1.txt",
        ]
    };

    for url in &cdn_endpoints {
        match client.get(*url).send().await {
            Ok(resp) => {
                let s = resp.status().as_u16();
                if resp.status().is_success() || s == 301 || s == 302 || s == 304 || s == 404 {
                    logger::debug(&format!("CDN connectivity OK via {}:{} -> {} [{}]", ip, port, url, s), "ProxyTester", None).ok();
                    return Ok(());
                }
            }
            Err(e) => {
                logger::debug(&format!("CDN probe failed for {} via {}:{}: {}", url, ip, port, e), "ProxyTester", None).ok();
                continue;
            }
        }
    }

    Err(anyhow!("Proxy cannot reach Discord CDN (cdn.discord.com/cdn.discordapp.com/media.discordapp.net)"))
}


// Perform a TCP-level HTTP CONNECT to the target host through the proxy to check tunnel support
async fn probe_http_connect(ip: &str, port: u16, target_host: &str) -> Result<()> {
    let addr_str = format!("{}:{}", ip, port);
    let socket_addr = match addr_str.parse::<SocketAddr>() {
        Ok(a) => a,
        Err(_) => return Err(anyhow!("Invalid proxy address {}", addr_str)),
    };

    // Connect to the proxy
    let conn = match timeout(Duration::from_secs(3), TcpStream::connect(socket_addr)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(anyhow!("TCP connect to proxy failed: {}", e)),
        Err(_) => return Err(anyhow!("TCP connect to proxy timed out")),
    };

    let mut stream = conn;

    // Build CONNECT request
    let connect_req = format!("CONNECT {}:443 HTTP/1.1\r\nHost: {}:443\r\nProxy-Connection: Keep-Alive\r\n\r\n", target_host, target_host);

    // Send CONNECT
    if let Err(e) = timeout(Duration::from_secs(2), stream.write_all(connect_req.as_bytes())).await {
        return Err(anyhow!("Failed to send CONNECT: timeout ({})", e));
    }

    // Read response (small buffer)
    let mut buf = [0u8; 1024];
    let n = match timeout(Duration::from_secs(3), stream.read(&mut buf)).await {
        Ok(Ok(n)) => n,
        Ok(Err(e)) => return Err(anyhow!("Failed to read CONNECT response: {}", e)),
        Err(_) => return Err(anyhow!("Timed out waiting for CONNECT response")),
    };

    let resp_str = String::from_utf8_lossy(&buf[..n]).to_string();
    if resp_str.starts_with("HTTP/1.1 200") || resp_str.starts_with("HTTP/1.0 200") {
        return Ok(());
    }

    Err(anyhow!("CONNECT failed or returned non-200: {}", resp_str.lines().next().unwrap_or("<empty>")))
}

// Lightweight, panic-free IP geolocation helper with timeout and fallback
#[derive(Debug, Clone, Default)]
struct GeoInfo {
    country: String,
    city: String,
    isp: String,
}

#[derive(Deserialize)]
struct IpApiResp {
    status: Option<String>,
    country: Option<String>,
    city: Option<String>,
    isp: Option<String>,
}

#[derive(Deserialize)]
struct IpWhoResp {
    success: Option<bool>,
    country: Option<String>,
    city: Option<String>,
    #[serde(default)]
    connection: Option<IpWhoConnection>,
}

#[derive(Deserialize)]
struct IpWhoConnection {
    isp: Option<String>,
}

async fn test_discord_connectivity(ip: &str, port: u16, proxy_type: &str) -> bool {
    // Test if proxy can reach Discord's API
    let client = match proxy_type.to_lowercase().as_str() {
        "socks4" | "socks5" => {
            let proxy_url = format!("socks5://{}:{}", ip, port);
            match reqwest::Proxy::all(&proxy_url) {
                Ok(proxy) => {
                    match reqwest::Client::builder()
                        .proxy(proxy)
                        .timeout(Duration::from_secs(6))
                        .danger_accept_invalid_certs(true)
                        .build() {
                        Ok(client) => client,
                        Err(_) => return false,
                    }
                },
                Err(_) => return false,
            }
        },
        _ => {
            // HTTP/HTTPS proxy
            let proxy_url = format!("http://{}:{}", ip, port);
            match reqwest::Proxy::all(&proxy_url) {
                Ok(proxy) => {
                    match reqwest::Client::builder()
                        .proxy(proxy)
                        .timeout(Duration::from_secs(6))
                        .danger_accept_invalid_certs(true)
                        .build() {
                        Ok(client) => client,
                        Err(_) => return false,
                    }
                },
                Err(_) => return false,
            }
        }
    };

    // Test 1: Try to reach Discord's CDN (should work if proxy can access internet)
    if let Ok(Ok(resp)) = tokio::time::timeout(Duration::from_secs(5), client.get("https://cdn.discordapp.com/attachments/1/1/1.txt").send()).await {
        if resp.status().is_success() || resp.status() == 404 {
            // 404 is fine, means we reached Discord's servers
            logger::debug(&format!("Proxy {}:{} can reach Discord CDN", ip, port), "ProxyTester", None).ok();
            return true;
        }
    }

    // Test 2: Try Discord's status API (lighter test)
    if let Ok(Ok(resp)) = tokio::time::timeout(Duration::from_secs(5), client.get("https://status.discord.com/api/v2/status.json").send()).await {
        if resp.status().is_success() {
            logger::debug(&format!("Proxy {}:{} can reach Discord status API", ip, port), "ProxyTester", None).ok();
            return true;
        }
    }

    // Test 3: Basic internet connectivity test
    if let Ok(Ok(resp)) = tokio::time::timeout(Duration::from_secs(5), client.get("https://httpbin.org/ip").send()).await {
        if resp.status().is_success() {
            logger::debug(&format!("Proxy {}:{} has internet access but may be blocked by Discord", ip, port), "ProxyTester", None).ok();
            return true; // Basic internet works, might work for Discord
        }
    }

    logger::debug(&format!("Proxy {}:{} failed all connectivity tests", ip, port), "ProxyTester", None).ok();
    false
}

async fn get_ip_geolocation(ip: &str) -> Result<GeoInfo> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;

    // First try ip-api.com
    let url = format!("http://ip-api.com/json/{}", ip);
    if let Ok(resp) = client.get(&url).send().await {
        if let Ok(data) = resp.json::<IpApiResp>().await {
            if data.status.as_deref() == Some("success") {
                return Ok(GeoInfo {
                    country: data.country.unwrap_or_else(|| "Unknown".into()),
                    city: data.city.unwrap_or_else(|| "Unknown".into()),
                    isp: data.isp.unwrap_or_else(|| "Unknown".into()),
                });
            }
        }
    }

    // Fallback to ipwho.is (no API key required)
    let url2 = format!("https://ipwho.is/{}", ip);
    let resp2 = client.get(&url2).send().await?;
    let data2: IpWhoResp = resp2.json().await?;
    if data2.success.unwrap_or(false) {
        let isp = data2
            .connection
            .as_ref()
            .and_then(|c| c.isp.clone())
            .unwrap_or_else(|| "Unknown".into());
        return Ok(GeoInfo {
            country: data2.country.unwrap_or_else(|| "Unknown".into()),
            city: data2.city.unwrap_or_else(|| "Unknown".into()),
            isp,
        });
    }

    Err(anyhow!("All geolocation services failed"))
}

// Command to start using a proxy
#[tauri::command]
async fn start_proxy(
    state: State<'_, Arc<AppState>>,
    ip: String,
    port: u16
) -> Result<String, String> {
    // Check if proxy is already running
    {
        let is_running = state.is_proxy_running.lock().await;
        if *is_running {
            return Err("Proxy is already running".to_string());
        }
    }
    
    // Find the proxy in our list
    let proxy_info = {
        let proxies = state.proxies.lock().await;
        proxies.iter()
            .find(|p| p.ip == ip && p.port == port)
            .cloned()
            .ok_or_else(|| format!("Proxy {}:{} not found", ip, port))?
    };
    
    // Verify the proxy can actually reach Discord before starting the server
    if proxy_info.is_working {
        logger::info(
            &format!("Testing Discord connectivity for proxy {}:{}", ip, port),
            "ProxyServer",
            None
        ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
        
        // Do a quick, permissive Discord connectivity probe (faster and avoids strict cert failures)
        match test_http_proxy_probe(&ip, port, &proxy_info.proxy_type).await {
            Ok(_) => {
                logger::info(
                    &format!("Proxy {}:{} confirmed working for Discord", ip, port),
                    "ProxyServer",
                    None
                ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));

                // CDN reachability (assets/chunks/media). For HTTP proxies, don't hard fail; warn and continue.
                match test_discord_cdn_connectivity(&ip, port, &proxy_info.proxy_type).await {
                    Ok(_) => {
                        logger::info(
                            &format!("Proxy {}:{} confirmed working for Discord CDN", ip, port),
                            "ProxyServer",
                            None
                        ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
                    }
                    Err(e) => {
                        if proxy_info.proxy_type.to_lowercase().starts_with("socks") {
                            // SOCKS proxies should fully support HTTPS tunneling; fail hard
                            return Err(format!("Proxy {}:{} failed CDN connectivity test: {}", ip, port, e));
                        } else {
                            logger::warning(
                                &format!("HTTP proxy {}:{} could not verify CDN connectivity; continuing. Some assets may fail to load. Error: {}", ip, port, e),
                                "ProxyServer",
                                None
                            ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
                        }
                    }
                }
            },
            Err(e) => {
                return Err(format!("Proxy {}:{} failed Discord connectivity test: {}", ip, port, e));
            }
        }
    } else {
        return Err(format!("Proxy {}:{} is not marked as working. Please test the proxy first.", ip, port));
    }
    
    // Set as active proxy
    {
        let mut active_proxy = state.active_proxy.lock().await;
        *active_proxy = Some(proxy_info.clone());
    }
    
    // Try to start the local proxy server on port 8080, or find an available port
    let local_ports = [8080, 8081, 8082, 8083, 8084, 8085];
    let mut selected_port = 0;
    let mut server_result = None;
    
    for &local_port in &local_ports {
        let addr = SocketAddr::from(([127, 0, 0, 1], local_port));
        
    // Build a reqwest client
    let _proxy_addr = format!("{}:{}", ip, port);
        
        // Create a proxy for the client (support SOCKS5 with remote DNS)
        let proxy_config = match proxy_info.proxy_type.as_str() {
            "socks5" => {
                let url = if let (Some(u), Some(p)) = (proxy_info.username.as_ref(), proxy_info.password.as_ref()) {
                    format!("socks5h://{}:{}@{}:{}", u, p, ip, port)
                } else {
                    format!("socks5h://{}:{}", ip, port)
                };
                logger::debug(
                    &format!("Creating SOCKS5 proxy client with URL: {}", url),
                    "ProxyServer",
                    None
                ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));

                match reqwest::Proxy::all(&url) {
                    Ok(p) => {
                        logger::debug(
                            &format!("Successfully created SOCKS5 proxy config for {}:{}", ip, port),
                            "ProxyServer",
                            None
                        ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
                        p
                    },
                    Err(e) => {
                        logger::error(
                            &format!("Failed to create SOCKS5 proxy for {}:{}", ip, port),
                            "ProxyServer",
                            Some(&e.to_string())
                        ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
                        continue;
                    }
                }
            }
            "socks4" => {
                // Reqwest doesn't natively support SOCKS4; attempt SOCKS5 as a fallback
                let url = if let (Some(u), Some(p)) = (proxy_info.username.as_ref(), proxy_info.password.as_ref()) {
                    format!("socks5h://{}:{}@{}:{}", u, p, ip, port)
                } else {
                    format!("socks5h://{}:{}", ip, port)
                };
                match reqwest::Proxy::all(&url) {
                    Ok(p) => p,
                    Err(e) => {
                        logger::warning(
                            &format!("SOCKS4 proxy not supported; fallback failed for {}:{}", ip, port),
                            "ProxyServer",
                            Some(&e.to_string())
                        ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
                        continue;
                    }
                }
            }
            _ => {
                let url = format!("http://{}:{}", ip, port);
                match reqwest::Proxy::all(&url) {
                    Ok(mut p) => {
                        if let Some(user) = proxy_info.username.as_ref() {
                            if let Some(pass) = proxy_info.password.as_ref() {
                                p = p.basic_auth(user, pass);
                            }
                        }
                        p
                    },
                    Err(e) => {
                        logger::error(
                            &format!("Failed to create HTTP proxy for {}:{}", ip, port),
                            "ProxyServer",
                            Some(&e.to_string())
                        ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
                        continue;
                    }
                }
            }
        };
        
    // Create service function
    let proxy_addr = format!("{}:{}", ip, port);
    let client = match reqwest::Client::builder()
        .proxy(proxy_config)
        .timeout(Duration::from_secs(30))  // Increased timeout
        .connect_timeout(Duration::from_secs(10))  // Connection timeout
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(10)
        .tcp_nodelay(true)
        .build() {
            Ok(c) => Arc::new(c),
            Err(e) => {
                logger::error(
                    &format!("Failed to build HTTP client for {}:{}", ip, port),
                    "ProxyServer",
                    Some(&e.to_string())
                ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
                continue;
            }
        };
        
        let proxy_type_ref = Arc::new(proxy_info.proxy_type.clone());
        
        // Create service function
        let state_ref = state.inner().clone();
        let make_svc = make_service_fn(move |_conn| {
            let client = client.clone();
            let proxy_addr = proxy_addr.clone();
            let proxy_type = proxy_type_ref.clone();
            let state_clone = state_ref.clone();
            
            async move {
                Ok::<_, hyper::Error>(service_fn(move |req: Request<Body>| {
                    let client = client.clone();
                    let proxy_addr = proxy_addr.clone();
                    let proxy_type = proxy_type.clone();
                    let state_clone = state_clone.clone();
                    
                    async move {
                        proxy_request(client, req, &proxy_addr, &proxy_type, state_clone).await
                    }
                }))
            }
        });
        
        // Try to bind to this port
        match Server::try_bind(&addr) {
            Ok(server) => {
                selected_port = local_port;
                server_result = Some(server.serve(make_svc));
                break;
            },
            Err(e) => {
                logger::warning(
                    &format!("Failed to bind to port {}", local_port),
                    "ProxyServer",
                    Some(&e.to_string())
                ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
                continue;
            }
        }
    }
    
    if selected_port == 0 {
        return Err("Failed to find an available port for the proxy server".to_string());
    }
    
    // Mark proxy as running
    {
        let mut is_running = state.is_proxy_running.lock().await;
        *is_running = true;
    }
    // Store selected port for navigation
    {
        let mut port_lock = state.current_proxy_port.lock().await;
        *port_lock = Some(selected_port);
    }
    
    // Start server in a new task
    let state_for_task = state.inner().clone();
    tokio::spawn(async move {
        let server = server_result.unwrap();
        
        if let Err(e) = server.await {
            logger::error(
                "Proxy server error",
                "ProxyServer",
                Some(&e.to_string())
            ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
            
            let mut is_running = state_for_task.is_proxy_running.lock().await;
            *is_running = false;
        }
    });
    
    Ok(format!("Proxy started on port {}", selected_port))
}

// Command to stop the proxy
#[tauri::command]
async fn stop_proxy(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let mut is_running = state.is_proxy_running.lock().await;
    if *is_running {
        *is_running = false;
        
        let mut active_proxy = state.active_proxy.lock().await;
        *active_proxy = None;
        let mut port_lock = state.current_proxy_port.lock().await;
        *port_lock = None;
        
        Ok("Proxy stopped".to_string())
    } else {
        Err("Proxy is not running".to_string())
    }
}

// Command to launch Discord through the proxy
#[tauri::command]
async fn launch_discord(window: Window, state: State<'_, Arc<AppState>>) -> Result<String, String> {
    // Check if proxy is running
    {
        let is_running = state.is_proxy_running.lock().await;
        if !*is_running {
            return Err("Proxy is not running. Please start a proxy first.".to_string());
        }
    }
    
    logger::info("Launching Discord in main window", "Discord", None)
        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
    
    // Navigate to the local proxy URL for Discord (use actual bound port)
    let port = {
        let p = state.current_proxy_port.lock().await;
        p.unwrap_or(8080)
    };
    let url = format!("http://localhost:{}", port);
    
    // Use our new navigation command
    navigate_to_url(window, url.to_string()).await
}

// Command to get the active proxy
#[tauri::command]
async fn get_active_proxy(state: State<'_, Arc<AppState>>) -> Result<Option<ProxyInfo>, String> {
    let active_proxy = state.active_proxy.lock().await;
    Ok(active_proxy.clone())
}

// Command to get proxy status
#[tauri::command]
async fn get_proxy_status(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    let is_running = state.is_proxy_running.lock().await;
    Ok(*is_running)
}

#[tauri::command]
async fn get_proxy_base_url(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let port = {
        let port_lock = state.current_proxy_port.lock().await;
        *port_lock
    };

    match port {
        Some(port) => Ok(format!("http://localhost:{}", port)),
        None => {
            logger::warning("No proxy port available, using default 8080", "ProxyServer", None)
                .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
            Ok("http://localhost:8080".to_string())
        }
    }
}

// Handle proxy requests
async fn proxy_request(
    client: Arc<reqwest::Client>,
    req: Request<Body>,
    _proxy_addr: &str,
    _proxy_type: &str,
    state: Arc<AppState>,
) -> Result<Response<Body>, hyper::Error> {
    // Check if proxy is still active
    let is_active = {
        let is_running = state.is_proxy_running.lock().await;
        *is_running
    };
    
    if !is_active {
        return Ok(Response::builder()
            .status(503)
            .body(Body::from("Proxy is not running"))
            .unwrap());
    }
    
    // Get the method, path and query from the original request
    // IMPORTANT: We need to extract and clone these values before consuming req
    let uri = req.uri();
    let path = uri.path().to_string(); // Clone the path
    let query = uri.query().unwrap_or("").to_string(); // Clone the query string
    let method = req.method().clone();
    
    // Enhanced logging for debugging
    logger::debug(
        &format!("Incoming request: {} {}{}", method, path, 
                if query.is_empty() { "".to_string() } else { format!("?{}", query) }),
        "ProxyServer", 
        None
    ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
    
    // ==================== STEP 1: HANDLE PREFLIGHT REQUESTS ====================
    // CRITICAL: For OPTIONS requests, don't even try to forward to Discord - respond immediately
    // Add a simple health check endpoint
    if path == "/health" {
        eprintln!("🔥 PROXY_REQUEST: Health check request");
        return Ok(Response::builder()
            .status(200)
            .header("Content-Type", "text/plain")
            .header("Access-Control-Allow-Origin", "*")
            .body(Body::from("Proxy server is running"))
            .unwrap());
    }

    if method == hyper::Method::OPTIONS {
        logger::info(
            &format!("Directly handling OPTIONS preflight for: {}", path),
            "ProxyServer", 
            None
        ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));

        // Read requested method/headers for echoing back
        let req_headers = req.headers();
        let allow_methods = req_headers
            .get("access-control-request-method")
            .cloned()
            .unwrap_or_else(|| hyper::header::HeaderValue::from_static("GET, POST, PUT, PATCH, DELETE, OPTIONS"));
        let allow_headers = req_headers
            .get("access-control-request-headers")
            .cloned()
            .unwrap_or_else(|| hyper::header::HeaderValue::from_static("*, content-type, authorization, x-super-properties, x-discord-locale, x-debug-options, x-track"));

        // Respond with 204 No Content for preflight requests - NEVER forward them
        return Ok(Response::builder()
            .status(204)
            .header("Access-Control-Allow-Origin", "*")
            .header("Access-Control-Allow-Methods", allow_methods)
            .header("Access-Control-Allow-Headers", allow_headers)
            .header("Access-Control-Allow-Credentials", "true")
            .header("Access-Control-Max-Age", "86400")
            .body(Body::empty())
            .unwrap());
    }
    
    // ==================== STEP 2: FORWARD LOGIN REQUESTS ====================
    // Forward login requests directly to Discord
    if path.contains("/api/v9/auth/login") && method == hyper::Method::POST {
        logger::info(
            "Forwarding login request to Discord",
            "ProxyServer",
            None
        ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
        
        // Add detailed debugging for login requests
        logger::debug(
            "Login request may require captcha - ensuring proper forwarding",
            "ProxyServer",
            None
        ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
    }
    
    // ==================== STEP 3: HANDLE ANALYTICS AND CAPTCHA ENDPOINTS ====================
    // Special handling for analytics/telemetry requests - Discord doesn't need these
    if path.contains("/api/v9/science") || path.contains("/api/v9/track") {
        logger::info(
            &format!("Handling analytics endpoint: {}", path),
            "ProxyServer",
            None
        ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
        
        // Just return a successful response for analytics - no need to forward them
        return Ok(Response::builder()
            .status(204) // No Content - success but nothing to return
            .header("Access-Control-Allow-Origin", "*")
            .header("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS")
            .header("Access-Control-Allow-Headers", "*")
            .header("Access-Control-Allow-Credentials", "true")
            .body(Body::empty())
            .unwrap());
    }
    
    // Handle timeout-prone status API requests with fallback
    if path.contains("/api/v2/scheduled-maintenances") || path.contains("/api/v2/incidents") {
        logger::info(
            &format!("Handling Discord status API request: {}", path),
            "ProxyServer",
            None
        ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
        
        // Return a fallback response to prevent infinite loading
        let fallback_response = if path.contains("scheduled-maintenances") {
            r#"{"scheduled_maintenances":[]}"#
        } else {
            r#"{"incidents":[]}"#
        };
        
        return Ok(Response::builder()
            .status(200)
            .header("Access-Control-Allow-Origin", "*")
            .header("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS")
            .header("Access-Control-Allow-Headers", "*")
            .header("Access-Control-Allow-Credentials", "true")
            .header("Content-Type", "application/json")
            .body(Body::from(fallback_response))
            .unwrap());
    }

    // IMPORTANT: Do NOT stub /api/v9/experiments.
    // The client needs the real fingerprint from Discord to authenticate properly.
    // We'll forward this endpoint normally; only if upstream fails will we rely on generic error handling below.
    
    // Special handling for hCaptcha requests
    if path.contains("hcaptcha") || path.contains("captcha") {
        logger::info(
            &format!("Detected hCaptcha-related request: {}", path),
            "ProxyServer",
            None
        ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
        
        // Ensure we forward these requests properly
        logger::debug(
            "Forwarding captcha request with special handling",
            "ProxyServer",
            None
        ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
    }
    
    // ==================== STEP 4: HANDLE WEBSOCKET UPGRADE REQUESTS ====================
    let is_websocket = req.headers().get("upgrade")
        .map(|v| v.as_bytes())
        .map(|v| v.eq_ignore_ascii_case(b"websocket"))
        .unwrap_or(false);
    
    if is_websocket {
        logger::info(
            &format!("WebSocket request detected for: {}", path),
            "ProxyServer",
            None
        ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));

            // Our client-side overrides should route all Discord WebSockets to /__ws?target=wss://...
            if path.starts_with("/__ws") {
                // Extract target from query string (URL-decoded)
                let target_opt = if !query.is_empty() {
                    form_urlencoded::parse(query.as_bytes())
                        .find(|(k, _)| k == "target")
                        .map(|(_, v)| v.into_owned())
                } else { None };

                let target_ws = match target_opt {
                    Some(u) => u,
                    None => {
                        logger::error(
                            "Missing target query parameter for websocket tunnel",
                            "ProxyServer",
                            None
                        ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
                        return Ok(Response::builder()
                            .status(400)
                            .header("Access-Control-Allow-Origin", "*")
                            .body(Body::from("Missing target query parameter"))
                            .unwrap());
                    }
                };

                // Add timeout handling for WebSocket connections
                logger::info(
                    &format!("Setting up WebSocket tunnel to: {}", target_ws),
                    "ProxyServer",
                    None
                ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));

                // Prepare upgrade response
                let ws_key = match req.headers().get("sec-websocket-key") {
                    Some(key) => key.clone(),
                    None => {
                        logger::error(
                            "WebSocket request missing Sec-WebSocket-Key header",
                            "ProxyServer",
                            None
                        ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
                        return Ok(Response::builder()
                            .status(400)
                            .header("Access-Control-Allow-Origin", "*")
                            .body(Body::from("WebSocket request missing Sec-WebSocket-Key header"))
                            .unwrap());
                    }
                };

                let accept_key = compute_websocket_accept(&ws_key);
                let ws_protocol = req.headers().get("sec-websocket-protocol").cloned();

                // Spawn task to bridge client <-> target after upgrade with timeout handling
                let spawn_req = req;
                let state_clone_for_ws = state.clone();
                tokio::spawn(async move {
                    match hyper::upgrade::on(spawn_req).await {
                        Ok(upgraded) => {
                            // Add timeout for WebSocket bridging
                            let bridge_result = tokio::time::timeout(
                                Duration::from_secs(300), // 5 minute timeout
                                bridge_websockets(upgraded, &target_ws, state_clone_for_ws)
                            ).await;
                            
                            match bridge_result {
                                Ok(Ok(())) => {
                                    let _ = logger::info(
                                        "WebSocket bridge completed successfully",
                                        "ProxyServer",
                                        None
                                    );
                                },
                                Ok(Err(e)) => {
                                    let _ = logger::error(
                                        &format!("WebSocket bridge error: {}", e),
                                        "ProxyServer",
                                        None
                                    );
                                },
                                Err(_) => {
                                    let _ = logger::warning(
                                        "WebSocket bridge timed out after 5 minutes",
                                        "ProxyServer",
                                        None
                                    );
                                }
                            }
                        },
                        Err(e) => {
                            let _ = logger::error(
                                &format!("WebSocket upgrade error: {}", e),
                                "ProxyServer",
                                None
                            );
                        }
                    }
                });

                let mut rb = Response::builder();
                rb = rb.status(101)
                    .header("Connection", "Upgrade")
                    .header("Upgrade", "websocket")
                    .header("Sec-WebSocket-Accept", accept_key)
                    .header("Sec-WebSocket-Version", "13")
                    .header("Access-Control-Allow-Origin", "*");
                if let Some(proto) = ws_protocol { rb = rb.header("Sec-WebSocket-Protocol", proto); }
                return Ok(rb.body(Body::empty()).unwrap());
            } else {
                // Not our tunnel endpoint; reject to avoid dangling upgrades
                return Ok(Response::builder()
                    .status(400)
                    .header("Access-Control-Allow-Origin", "*")
                    .body(Body::from("Unsupported websocket path. Use /__ws?target=wss://..."))
                    .unwrap());
            }
        }
        
    // ==================== STEP 5: BUILD THE TARGET URL ====================
    // Always use HTTPS for Discord URLs - select target host via header or path analysis
    let mut target_base = String::from("https://discord.com");

    // Cloudflare challenge platform script can fail through certain proxies; short-circuit with minimal script
    if path == "/cdn-cgi/challenge-platform/scripts/jsd/main.js" {
        logger::warning(
            "Serving minimal stub for Cloudflare challenge script to avoid 502",
            "ProxyServer",
            None
        ).ok();
        let body = "// dc-proxy stub: challenge script bypassed\n";
        return Ok(Response::builder()
            .status(200)
            .header("Content-Type", "application/javascript")
            .header("Access-Control-Allow-Origin", "*")
            .body(Body::from(body))
            .unwrap());
    }
    
    // List of known CDN path prefixes. These are for user content, avatars, etc.
    const CDN_PATHS: &[&str] = &[
        "/attachments/", "/avatars/", "/banners/", "/icons/", "/emojis/",
        "/stickers/", "/app-assets/", "/app-icons/", "/team-icons/", "/assets/content/",
        "/role-icons/", "/channel-icons/", "/guild-events/", "/discovery-splashes/",
        "/avatar-decoration-presets/", "/splashes/", "/changelogs/", "/media/", "/assets/collectibles/",
    ];

    // Detect target subdomain from common Discord API patterns
    // Note: The default target_base is "https://discord.com"
    if path.contains("/api/v2/scheduled-maintenances") || path.contains("/api/v2/incidents") {
        target_base = String::from("https://status.discord.com");
    } else if path.starts_with("/cdn-cgi/") {
        // Cloudflare challenge and platform scripts live under the zone root, not the CDN subdomain
        target_base = String::from("https://discord.com");
    } else if CDN_PATHS.iter().any(|p| path.starts_with(p)) {
        // Requests for user content, avatars, icons, etc., should go to the CDN.
        // Using cdn.discordapp.com as it's the most common host for these assets.
        target_base = String::from("https://cdn.discordapp.com");
    } else if path.contains("/store/") || path.contains("/nitro/") {
        target_base = String::from("https://discord.com");
    }
    
    // Special case for the root path
    if path == "/" || path == "/discord" {
        target_base.push_str("/app");
    } else {
        // All other paths are passed through
        target_base.push_str(&path);
        if !query.is_empty() {
            target_base.push_str(&format!("?{}", query));
        }
    }
    
    logger::debug(
        &format!("Forwarding to: {}", target_base),
        "ProxyServer",
        None
    ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
    
    // ==================== STEP 6: PREPARE THE REQUEST ====================
    // Now it's safe to consume req since we've captured all we need from it
    let (parts, body) = req.into_parts();
    
    // Convert request method
    let req_method = match parts.method.as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "DELETE" => reqwest::Method::DELETE,
        "HEAD" => reqwest::Method::HEAD,
        "OPTIONS" => reqwest::Method::OPTIONS,
        "CONNECT" => reqwest::Method::CONNECT,
        "PATCH" => reqwest::Method::PATCH,
        "TRACE" => reqwest::Method::TRACE,
        _ => reqwest::Method::GET,
    };
    
    // Create request builder
    // Read custom header for target host before consuming headers
    let mut target_host: Option<String> = None;
    for (name, value) in parts.headers.iter() {
        if name.as_str().eq_ignore_ascii_case("x-discord-target-host") {
            if let Ok(vs) = value.to_str() { target_host = Some(vs.to_string()); }
        }
    }
    let mut effective_url = target_base;
    if let Some(host) = target_host {
        // Replace base host while preserving scheme and path
        if let Ok(mut u) = url::Url::parse(&effective_url) {
            let _ = u.set_host(Some(&host));
            effective_url = u.to_string();
        }
    }

    let mut req_builder = client.request(req_method, &effective_url);
    
    // ==================== STEP 7: ADD REQUIRED HEADERS ====================
    // Extract target host for proper Origin/Referer headers
    let target_origin = if let Ok(parsed_url) = url::Url::parse(&effective_url) {
        if let Some(host) = parsed_url.host_str() {
            format!("https://{}", host)
        } else {
            "https://discord.com".to_string()
        }
    } else {
        "https://discord.com".to_string()
    };
    
    // Common Discord headers that help requests succeed
    // Prefer the client's User-Agent if provided, else fall back to a modern UA
    let mut client_user_agent: Option<String> = None;
    let mut client_origin: Option<String> = None;
    for (name, value) in parts.headers.iter() {
        if name.as_str().eq_ignore_ascii_case("user-agent") {
            if let Ok(vs) = value.to_str() { client_user_agent = Some(vs.to_string()); }
        }
        if name.as_str().eq_ignore_ascii_case("origin") {
            if let Ok(vs) = value.to_str() { client_origin = Some(vs.to_string()); }
        }
    }
    let ua_value = client_user_agent.as_deref().unwrap_or("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36");
    
    // For login requests, preserve the client's origin (localhost:8080) to avoid CORS issues
    let origin_to_use = if path.contains("/api/v9/auth/login") && client_origin.is_some() {
        client_origin.clone().unwrap()
    } else {
        target_origin.clone()
    };
    
    let referer_to_use = if path.contains("/api/v9/auth/login") && client_origin.is_some() {
        format!("{}/login", client_origin.unwrap())
    } else {
        format!("{}/", target_origin)
    };
    
    req_builder = req_builder
        .header("User-Agent", ua_value)
        .header("Origin", &origin_to_use)
        .header("Referer", &referer_to_use)
        // Request identity encoding to keep body handling predictable through the proxy
        .header("Accept-Encoding", "identity");
    
    // For API requests, add more specific headers
    if path.starts_with("/api/") {
        req_builder = req_builder
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("Sec-Fetch-Dest", "empty")
            .header("Sec-Fetch-Mode", "cors")
            .header("Sec-Fetch-Site", "same-origin");

        // Prefer HTTP/1.1 for Discord API calls to avoid rare HTTP/2 reset issues
        req_builder = req_builder.version(reqwest::Version::HTTP_11);
        
        // Authentication endpoints need special handling
        if path.contains("/api/v9/auth/") || path.contains("/api/v9/login") || 
           path.contains("location-metadata") || path.contains("conditional/start") ||
           path.contains("experiments") {
            req_builder = req_builder.header("Referer", "https://discord.com/login");
            
            // Add debug log for auth endpoints
            logger::info(
                &format!("Auth-related endpoint detected: {}", path),
                "ProxyServer",
                None
            ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
            
            // CRITICAL: For auth/login POST requests, ensure correct content-type
            if path.contains("/auth/login") && method == hyper::Method::POST {
                logger::info(
                    "Auth login POST detected, ensuring content-type is application/json",
                    "ProxyServer",
                    None
                ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
                
                // Ensure complete set of headers for login requests
                // Build super properties based on (possible) client UA
                let login_super_props = build_login_super_properties(client_user_agent.as_deref());
                // Synthesize X-Track to match same-origin expectations
                let x_track = {
                    let payload = serde_json::json!({
                        "os": "Windows",
                        "browser": "Chrome",
                        "device": "",
                        "system_locale": "en-US",
                        "referrer": "https://discord.com/",
                        "referring_domain": "discord.com",
                        "referrer_current": "https://discord.com/login",
                        "referring_domain_current": "discord.com",
                        "release_channel": "stable",
                    }).to_string();
                    general_purpose::STANDARD.encode(payload.as_bytes())
                };

                req_builder = req_builder
                    .header("Content-Type", "application/json")
                    .header("Accept", "application/json")
                    .header("Host", "discord.com")  // Important for hCaptcha
                    // Note: Origin and Referer are already set above to preserve client's localhost for CORS
                    .header("X-Super-Properties", login_super_props)
                        .header("X-Track", x_track)
                        .header("X-Debug-Options", "bugReporterEnabled")
                        .header("sec-ch-ua", "\"Chromium\";v=\"115\", \"Not/A)Brand\";v=\"99\"")
                        .header("sec-ch-ua-platform", "\"Windows\"")
                        .header("sec-fetch-dest", "empty")
                        .header("sec-fetch-mode", "cors")
                        .header("sec-fetch-site", "same-origin");

                    // Synthesize X-Track to match discord.com origin and referer
                    let track = serde_json::json!({
                        "os": "Windows",
                        "browser": "Chrome",
                        "device": "",
                        "system_locale": "en-US",
                        "browser_user_agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36",
                        "browser_version": "91.0.4472.124",
                        "os_version": "10",
                        "referrer": "https://discord.com",
                        "referring_domain": "discord.com",
                        "referrer_current": "https://discord.com/login",
                        "referring_domain_current": "discord.com",
                        "release_channel": "stable",
                        "client_build_number": 263156,
                        "client_event_source": serde_json::Value::Null
                    });
                    let x_track = general_purpose::STANDARD.encode(track.to_string());
                    req_builder = req_builder.header("X-Track", x_track);
                    
                    // Log the forwarded login request
                    logger::info(
                        "Forwarding login request with all required headers",
                        "ProxyServer",
                        None
                    ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
            }
        }
    }
    
    // Copy most original headers to the outgoing request to preserve client behavior
    // Skip hop-by-hop headers and let reqwest set Host/Content-Length/Transfer-Encoding as needed.
    let hop_by_hop = [
        "connection",
        "keep-alive",
        "proxy-authorization",
        "proxy-authenticate",
        "te",
        "trailers",
        "transfer-encoding",
        "upgrade",
        "proxy-connection",
        "expect",
    ];

    if !path.contains("/api/v9/auth/login") {
        for (name, value) in parts.headers.iter() {
        let name_l = name.as_str().to_lowercase();

        // Skip hop-by-hop and sensitive headers we explicitly set for Discord
        if hop_by_hop.iter().any(|h| h == &name_l.as_str())
            || name_l == "host"
            || name_l == "content-length"
            || name_l == "origin"
            || name_l == "referer"
            || name_l == "sec-fetch-site"
            || name_l == "sec-fetch-mode"
            || name_l == "sec-fetch-dest"
            || name_l == "accept-encoding" // we force identity
        {
            continue;
        }

        // Safely copy header values
        if let Ok(value_str) = value.to_str() {
            req_builder = req_builder.header(name.as_str(), value_str);
        }
        }
    } else {
        // For auth/login, forward a SAFE whitelist of client-provided headers that Discord expects
        // (we still set Origin/Referer/Host to discord.com explicitly above).
        const LOGIN_WHITELIST: &[&str] = &[ 
            "x-fingerprint",
            "x-context-properties",
            // DO NOT forward x-track or x-super-properties from client; we synthesize safe values
            "x-discord-locale",
            "x-discord-timezone",
            "user-agent",
            "dnt",
            "sec-ch-ua",
            "sec-ch-ua-platform",
            "x-debug-options",
            // captcha-related headers if present
            "x-captcha-key",
            "x-captcha-rqtoken",
            "x-captcha-provider",
        ];

        for (name, value) in parts.headers.iter() {
            let name_l = name.as_str().to_lowercase();

            // Skip hop-by-hop and restricted headers
            if hop_by_hop.iter().any(|h| h == &name_l.as_str())
                || name_l == "host"
                || name_l == "content-length"
                // Note: we preserve origin for login requests above, don't filter it out
                || name_l == "referer" // we enforce https://discord.com/login
                || name_l == "sec-fetch-site" // enforced above
                || name_l == "sec-fetch-mode"
                || name_l == "sec-fetch-dest"
                || name_l == "accept-encoding" // we force identity
            {
                continue;
            }

            // Only forward whitelisted headers for login
            if LOGIN_WHITELIST.iter().any(|h| h.eq_ignore_ascii_case(&name_l)) {
                if let Ok(value_str) = value.to_str() {
                    req_builder = req_builder.header(name.as_str(), value_str);
                }
            }
        }
    }
    
    // ==================== STEP 8: SEND THE REQUEST ====================
    // Convert hyper body to bytes for reqwest
    let body_bytes = match hyper::body::to_bytes(body).await {
        Ok(bytes) => bytes,
        Err(e) => {
            logger::error(
                "Error reading request body", 
                "ProxyServer", 
                Some(&e.to_string())
            ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
            
            return Ok(Response::builder()
                .status(500)
                .body(Body::from(format!("Error reading request body: {}", e)))
                .unwrap());
        }
    };
    
    // Add the body to the request builder only when appropriate.
    // Avoid attaching a body to GET/HEAD/OPTIONS which can trigger server-side connection resets.
    let method_is_safe = parts.method == hyper::Method::GET || parts.method == hyper::Method::HEAD || parts.method == hyper::Method::OPTIONS;
    if !method_is_safe && !body_bytes.is_empty() {
        req_builder = req_builder.body(body_bytes.to_vec());
    }
    
    // Send the request
    let response = match req_builder.send().await {
        Ok(resp) => resp,
        Err(e) => {
            // Enhanced error logging for Discord connectivity issues
            let (error_type, error_msg) = if e.is_timeout() {
                ("TIMEOUT", format!("Request timeout when connecting to Discord through proxy: {}", e))
            } else if e.is_connect() {
                ("CONNECTION_FAILED", format!("Cannot establish connection to Discord through proxy: {}", e))
            } else if e.is_request() {
                ("REQUEST_FAILED", format!("Request formation failed for Discord through proxy: {}", e))
            } else if e.to_string().contains("dns") || e.to_string().contains("resolve") {
                ("DNS_RESOLUTION", format!("DNS resolution failed through proxy: {}", e))
            } else if e.to_string().contains("certificate") || e.to_string().contains("tls") || e.to_string().contains("ssl") {
                ("TLS_SSL_ERROR", format!("TLS/SSL handshake failed through proxy: {}", e))
            } else if e.to_string().contains("proxy") {
                ("PROXY_ERROR", format!("Proxy-specific error: {}", e))
            } else {
                ("UNKNOWN", format!("Unknown error connecting to Discord through proxy: {}", e))
            };
            
            // Get proxy info for better debugging
            let proxy_info = {
                let active_proxy = state.active_proxy.lock().await;
                if let Some(proxy) = &*active_proxy {
                    format!("{}:{} ({})", proxy.ip, proxy.port, proxy.proxy_type)
                } else {
                    "Unknown".to_string()
                }
            };
            
            logger::error(
                &format!("Proxy connection failed [{}]: {} | Proxy: {} | Target: {}", 
                    error_type, error_msg, proxy_info, effective_url), 
                "ProxyServer", 
                None
            ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
            
            // Provide specific error message based on error type
            let user_msg = match error_type {
                "TIMEOUT" => format!("Proxy timeout: Your proxy at {} is too slow or unresponsive. Check proxy server status.", proxy_info),
                "CONNECTION_FAILED" => format!("Connection failed: Cannot connect through proxy at {}. Check if proxy server is running and accessible.", proxy_info),
                "DNS_RESOLUTION" => format!("DNS error: Proxy at {} cannot resolve Discord domains. Check proxy DNS configuration.", proxy_info),
                "TLS_SSL_ERROR" => format!("SSL/TLS error: Proxy at {} has certificate/encryption issues. Check proxy SSL configuration.", proxy_info),
                "PROXY_ERROR" => format!("Proxy error: Issue with proxy server at {}. Check proxy configuration and logs.", proxy_info),
                _ => format!("Proxy at {} cannot reach Discord. Check proxy server configuration and network connectivity.", proxy_info)
            };
            
            return Ok(Response::builder()
                .status(502)
                .header("Access-Control-Allow-Origin", "*")
                .header("Access-Control-Allow-Methods", "GET, POST, OPTIONS, PUT, DELETE, PATCH")
                .header("Access-Control-Allow-Headers", "*")
                .header("Access-Control-Allow-Credentials", "true")
                .body(Body::from(user_msg))
                .unwrap());
        }
    };
    
    // ==================== STEP 9: PREPARE THE RESPONSE ====================
    // Get the status code and response body
    let status = response.status().as_u16();
    let mut builder = Response::builder().status(status);
    
    // Log the response for debugging
    logger::debug(
    &format!("Response from {}: {} {}", 
        effective_url, 
                status,
                response.status().canonical_reason().unwrap_or("")),
        "ProxyServer",
        None
    ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
    
    // Add extra logging for login endpoints
    if path.contains("/api/v9/auth/login") {
        let status_desc = if status >= 400 {
            "ERROR"
        } else {
            "SUCCESS"
        };
        
        logger::info(
            &format!("Login response: {} ({})", status_desc, status),
        "ProxyServer",
        None
    ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
    }
    
    // ==================== STEP 10: ADD CORS HEADERS TO RESPONSE ====================
    // Add CORS headers to the response
    let headers = builder.headers_mut().unwrap();
    
    // Always add permissive CORS headers to all responses
    headers.insert(
        "Access-Control-Allow-Origin", 
        hyper::header::HeaderValue::from_static("*")
    );
    
    headers.insert(
        "Access-Control-Allow-Methods", 
        hyper::header::HeaderValue::from_static("GET, POST, OPTIONS, PUT, DELETE, PATCH")
    );
    
    headers.insert(
        "Access-Control-Allow-Headers", 
        hyper::header::HeaderValue::from_static("*")
    );
    
    headers.insert(
        "Access-Control-Allow-Credentials", 
        hyper::header::HeaderValue::from_static("true")
    );
    
        headers.insert(
            "Access-Control-Expose-Headers",
            hyper::header::HeaderValue::from_static("*")
        );
    
    // ==================== STEP 11: COPY RESPONSE HEADERS AND CHECK CONTENT TYPE ====================
    // Extract content-type for HTML detection before consuming the response
    let content_type = response.headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    
    // Check if this is an HTML response
    let is_html = content_type.contains("text/html");
    
    // Check if this is the Discord app HTML (login page or app)
    let is_discord_app = is_html && (
        path == "/app" || 
        path == "/login" || 
        path == "/register" || 
        path == "/" || 
        path == "/discord"
    );
    
    // Copy the headers from the reqwest response
    // Clone headers first to avoid borrowing conflicts
    let response_headers: Vec<(String, Vec<u8>)> = response.headers()
        .iter()
        .map(|(name, value)| (name.to_string(), value.as_bytes().to_vec()))
        .collect();

    for (name, value_bytes) in response_headers {
        let name_l = name.to_ascii_lowercase();

        // Skip headers that we already set
        if name_l == "access-control-allow-origin" ||
           name_l == "access-control-allow-methods" ||
           name_l == "access-control-allow-headers" ||
           name_l == "access-control-allow-credentials" ||
           name_l == "access-control-expose-headers" {
            continue;
        }

        // Skip security headers that might cause issues
        if name_l == "content-security-policy" ||
           name_l == "x-frame-options" ||
           name_l == "x-content-type-options" {
            continue;
        }

        // IMPORTANT: We may modify bodies (HTML injection) and/or reqwest may have already
        // decompressed the payload. Strip content-length/encoding and transfer-encoding to avoid
        // browser-side mismatches that can lead to net::ERR_CONNECTION_RESET.
        if name_l == "content-length" || name_l == "content-encoding" || name_l == "transfer-encoding" {
            continue;
        }

        // Copy all other headers
        if let Ok(hv) = hyper::header::HeaderValue::from_bytes(&value_bytes) {
            if let Ok(header_name) = hyper::header::HeaderName::from_bytes(name.as_bytes()) {
                headers.insert(header_name, hv);
            }
        }
    }
    
    // ==================== STEP 12: READ AND MODIFY RESPONSE BODY ====================
    // Now we can safely consume the response with bytes()
    let resp_bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(e) => {
            logger::error(
                &format!("Error reading response body: {}", e), 
                "ProxyServer", 
                None
            ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
            
            return Ok(Response::builder()
                .status(502)
                .header("Access-Control-Allow-Origin", "*")
                .header("Access-Control-Allow-Methods", "GET, POST, OPTIONS, PUT, DELETE, PATCH")
                .header("Access-Control-Allow-Headers", "*")
                .header("Access-Control-Allow-Credentials", "true")
                .body(Body::from(format!("Error reading response body: {}", e)))
                .unwrap());
        }
    };
    
    // For login requests, log the response body details
    if path.contains("/api/v9/auth/login") && status >= 400 {
        if let Ok(body_str) = String::from_utf8(resp_bytes.to_vec()) {
            if body_str.contains("captcha") {
                logger::info(
                    &format!("Login response contains captcha requirements: {}", 
                        body_str.chars().take(200).collect::<String>()),
            "ProxyServer",
            None
        ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
                                } else {
                logger::info(
                    &format!("Login error response: {}", 
                        body_str.chars().take(200).collect::<String>()),
                    "ProxyServer",
                    None
                ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
            }
        }
    }
    
    // If this is Discord HTML, inject our script
    if is_discord_app {
        logger::debug(
            "Detected Discord app page, injecting script",
            "ProxyServer",
            None
        ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
        
        // Convert response body to string
        if let Ok(body_str) = String::from_utf8(resp_bytes.to_vec()) {
            // Inject our script into the HTML
            let modified_html = inject_scripts_into_html(&body_str);
            
            // Return the modified HTML
            return Ok(builder.body(Body::from(modified_html)).unwrap());
        }
    }
    
    // For non-HTML responses, return the original body
    Ok(builder.body(Body::from(resp_bytes)).unwrap())
}

// Bridge upgraded client connection to target websocket server with enhanced error handling
async fn bridge_websockets(upgraded: Upgraded, target_url: &str, state: Arc<AppState>) -> anyhow::Result<()> {
    logger::info(
        &format!("Starting WebSocket bridge to: {}", target_url),
        "ProxyServer",
        None
    ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
    
    // Build client-side WebSocket from upgraded stream
    let client_ws = WebSocketStream::from_raw_socket(upgraded, WsRole::Server, None).await;

    // Boxed trait-object types so TLS and non-TLS WebSocketStream variants can be handled uniformly
    type WsSinkDyn = Pin<Box<dyn futures::Sink<WsMessage, Error = WsError> + Send>>;
    type WsStreamDyn = Pin<Box<dyn futures::Stream<Item = Result<WsMessage, WsError>> + Send>>;

    // Determine if we should use TLS-over-TCP (for wss) or plain websocket
    let active_proxy = { state.active_proxy.lock().await.clone() };

    // Prepare remote sink/stream; wrap sink in Arc<Mutex<...>> so we can use it from both tasks
    let (rem_tx, mut rem_rx): (Arc<tokio::sync::Mutex<WsSinkDyn>>, WsStreamDyn) = if target_url.starts_with("wss://") {
        // Parse host
        let url = url::Url::parse(target_url).map_err(|_| anyhow!("Invalid target URL"))?;
        let host = url.host_str().ok_or_else(|| anyhow!("Invalid target URL host"))?;
        let port = url.port().unwrap_or(443);

    // Prepare TLS connector (native) with comprehensive settings for Discord gateway
    let native = NativeTlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .use_sni(true)
        .min_protocol_version(Some(native_tls::Protocol::Tlsv12))
        .build()?;

        // If proxy present, CONNECT through proxy, otherwise direct TCP
        // Prepare header_buf in outer scope so leftover bytes are available after tcp_sock is created
        let mut header_buf: Vec<u8> = Vec::new();
        let tcp_sock = if let Some(proxy) = active_proxy {
            let proxy_addr = format!("{}:{}", proxy.ip, proxy.port);
            // Connect to proxy
            match tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&proxy_addr)).await {
                Ok(Ok(mut sock)) => {
                    // If SOCKS5, perform a minimal handshake to the target host
                    let ptype = proxy.proxy_type.to_lowercase();
                    if ptype.starts_with("socks5") {
                        // Greeting: version 5, 1 method, no-auth (0x00)
                        let greet = [0x05u8, 0x01, 0x00];
                        tokio::time::timeout(Duration::from_secs(3), sock.write_all(&greet)).await.map_err(|_| anyhow!("SOCKS5 greet timeout"))?.map_err(|e| anyhow!(e.to_string()))?;
                        let mut resp = [0u8; 2];
                        tokio::time::timeout(Duration::from_secs(3), sock.read_exact(&mut resp)).await.map_err(|_| anyhow!("SOCKS5 greet read timeout"))?.map_err(|e| anyhow!(e.to_string()))?;
                        if resp != [0x05, 0x00] { return Err(anyhow!("SOCKS5 no-auth not accepted")); }

                        // CONNECT request: VER=5, CMD=1, RSV=0, ATYP=3 (domain), LEN, HOST, PORT
                        let host_bytes = host.as_bytes();
                        if host_bytes.len() > 255 { return Err(anyhow!("Host too long for SOCKS5")); }
                        let mut req = Vec::with_capacity(7 + host_bytes.len());
                        req.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, host_bytes.len() as u8]);
                        req.extend_from_slice(host_bytes);
                        req.extend_from_slice(&[(port >> 8) as u8, (port & 0xff) as u8]);
                        tokio::time::timeout(Duration::from_secs(5), sock.write_all(&req)).await.map_err(|_| anyhow!("SOCKS5 connect write timeout"))?.map_err(|e| anyhow!(e.to_string()))?;

                        // Response: VER=5, REP=0 for success, RSV=0, ATYP, BND.ADDR, BND.PORT
                        let mut head = [0u8; 4];
                        tokio::time::timeout(Duration::from_secs(5), sock.read_exact(&mut head)).await.map_err(|_| anyhow!("SOCKS5 connect read timeout"))?.map_err(|e| anyhow!(e.to_string()))?;
                        if head[1] != 0x00 { return Err(anyhow!(format!("SOCKS5 connect failed with code {}", head[1]))); }
                        let atyp = head[3];
                        // Read bound address depending on ATYP
                        let addr_len = match atyp { 0x01 => 4, 0x03 => { let mut l=[0u8;1]; tokio::time::timeout(Duration::from_secs(3), sock.read_exact(&mut l)).await.map_err(|_| anyhow!("SOCKS5 addr len timeout"))?.map_err(|e| anyhow!(e.to_string()))?; l[0] as usize }, 0x04 => 16, _ => 0 };
                        if addr_len > 0 {
                            let mut skip = vec![0u8; addr_len];
                            tokio::time::timeout(Duration::from_secs(3), sock.read_exact(&mut skip)).await.map_err(|_| anyhow!("SOCKS5 addr read timeout"))?.map_err(|e| anyhow!(e.to_string()))?;
                        }
                        // Read port
                        let mut skip_port = [0u8; 2];
                        tokio::time::timeout(Duration::from_secs(3), sock.read_exact(&mut skip_port)).await.map_err(|_| anyhow!("SOCKS5 port read timeout"))?.map_err(|e| anyhow!(e.to_string()))?;

                        // SOCKS established; return socket for TLS handshake
                        sock
                    } else {
                        // Default: HTTP CONNECT
                        let connect_req = format!("CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\nProxy-Connection: Keep-Alive\r\n\r\n", host, port, host, port);
                        tokio::time::timeout(Duration::from_secs(5), sock.write_all(connect_req.as_bytes())).await.map_err(|_| anyhow!("Failed to send CONNECT to proxy"))?.map_err(|e| anyhow!(e.to_string()))?;

                        // Read response until end of headers (CRLFCRLF). Some proxies send larger header blocks so loop until found or limit reached.
                        let mut tmp = [0u8; 512];
                        let mut total_read = 0usize;
                        loop {
                            let n = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut tmp)).await.map_err(|_| anyhow!("No response from proxy CONNECT"))?.map_err(|e| anyhow!(e.to_string()))?;
                            if n == 0 { break; }
                            header_buf.extend_from_slice(&tmp[..n]);
                            total_read += n;
                            if header_buf.windows(4).any(|w| w == b"\r\n\r\n") || total_read > 16 * 1024 { break; }
                        }

                        let header_str = String::from_utf8_lossy(&header_buf).to_string();
                        if !header_str.starts_with("HTTP/1.1 200") && !header_str.starts_with("HTTP/1.0 200") {
                            logger::error(&format!("Proxy CONNECT failed, response: {}", header_str.lines().next().unwrap_or("<empty>")), "ProxyServer", None).ok();
                            return Err(anyhow!("Proxy CONNECT failed"));
                        }

                        sock
                    }
                },
                Ok(Err(e)) => return Err(anyhow!(format!("TCP connect to proxy {} failed: {}", proxy_addr, e))),
                Err(_) => return Err(anyhow!("Timeout connecting to proxy")),
            }
        } else {
            // Direct TCP
            let target_addr = format!("{}:{}", host, port);
            match tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&target_addr)).await {
                Ok(Ok(sock)) => sock,
                Ok(Err(e)) => return Err(anyhow!(format!("TCP connect to target failed: {}", e))),
                Err(_) => return Err(anyhow!("Timeout connecting to target")),
            }
        };

        // Perform TLS handshake and websocket client handshake. If the proxy returned
        // leftover bytes after headers, wrap them into a CombinedStream so the TLS
        // handshake consumes them first.
        let leftover_pos = header_buf.windows(4).position(|w| w == b"\r\n\r\n");
        if let Some(pos) = leftover_pos {
            let idx = pos + 4;
            let leftover_vec = if idx < header_buf.len() { header_buf[idx..].to_vec() } else { Vec::new() };
                if !leftover_vec.is_empty() {
                logger::info(&format!("Proxy CONNECT returned {} leftover bytes after headers; chaining into TLS stream", leftover_vec.len()), "ProxyServer", None).ok();

                struct CombinedStream {
                    leftover: std::io::Cursor<Vec<u8>>,
                    sock: TcpStream,
                }

                use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
                use std::task::{Context, Poll};
                use std::pin::Pin;

                impl AsyncRead for CombinedStream {
                    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
                        if (self.leftover.position() as usize) < self.leftover.get_ref().len() {
                            let pos = self.leftover.position() as usize;
                            let rem = &self.leftover.get_ref()[pos..];
                            let to_copy = rem.len().min(buf.remaining());
                            buf.put_slice(&rem[..to_copy]);
                            self.leftover.set_position((pos + to_copy) as u64);
                            return Poll::Ready(Ok(()));
                        }
                        Pin::new(&mut self.sock).poll_read(cx, buf)
                    }
                }

                impl AsyncWrite for CombinedStream {
                    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<std::io::Result<usize>> {
                        Pin::new(&mut self.sock).poll_write(cx, data)
                    }

                    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                        Pin::new(&mut self.sock).poll_flush(cx)
                    }

                    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                        Pin::new(&mut self.sock).poll_shutdown(cx)
                    }
                }

                impl std::marker::Unpin for CombinedStream {}

                let combined = CombinedStream { leftover: std::io::Cursor::new(leftover_vec), sock: tcp_sock };
                logger::info(&format!("Attempting WebSocket handshake to {} with combined stream", target_url), "ProxyServer", None).ok();
                let (ws, resp) = tokio::time::timeout(
                    Duration::from_secs(15),
                    tokio_tungstenite::client_async_tls_with_config(target_url, combined, None, Some(tokio_tungstenite::Connector::NativeTls(native.clone())))
                ).await
                .map_err(|_| anyhow!("WebSocket handshake timeout (15s)"))?
                .map_err(|e| anyhow!(format!("WebSocket handshake failed: {}", e)))?;
                logger::info(&format!("WebSocket handshake successful. Response status: {:?}", resp.status()), "ProxyServer", None).ok();
                let (sink, stream) = ws.split();
                (Arc::new(tokio::sync::Mutex::new(Box::pin(sink) as WsSinkDyn)), Box::pin(stream) as WsStreamDyn)
            } else {
                logger::info(&format!("Attempting WebSocket handshake to {} with direct stream", target_url), "ProxyServer", None).ok();
                let (ws, resp) = tokio::time::timeout(
                    Duration::from_secs(15),
                    tokio_tungstenite::client_async_tls_with_config(target_url, tcp_sock, None, Some(tokio_tungstenite::Connector::NativeTls(native.clone())))
                ).await
                .map_err(|_| anyhow!("WebSocket handshake timeout (15s)"))?
                .map_err(|e| anyhow!(format!("WebSocket handshake failed: {}", e)))?;
                logger::info(&format!("WebSocket handshake successful. Response status: {:?}", resp.status()), "ProxyServer", None).ok();
                let (sink, stream) = ws.split();
                (Arc::new(tokio::sync::Mutex::new(Box::pin(sink) as WsSinkDyn)), Box::pin(stream) as WsStreamDyn)
            }
        } else {
            logger::info(&format!("Attempting WebSocket handshake to {} without leftover data", target_url), "ProxyServer", None).ok();
            let (ws, resp) = tokio::time::timeout(
                Duration::from_secs(15),
                tokio_tungstenite::client_async_tls_with_config(target_url, tcp_sock, None, Some(tokio_tungstenite::Connector::NativeTls(native.clone())))
            ).await
            .map_err(|_| anyhow!("WebSocket handshake timeout (15s)"))?
            .map_err(|e| anyhow!(format!("WebSocket handshake failed: {}", e)))?;
            logger::info(&format!("WebSocket handshake successful. Response status: {:?}", resp.status()), "ProxyServer", None).ok();
            let (sink, stream) = ws.split();
            (Arc::new(tokio::sync::Mutex::new(Box::pin(sink) as WsSinkDyn)), Box::pin(stream) as WsStreamDyn)
        }
    } else {
        // Plain ws - for now keep the old approach since it's more complex to implement custom headers for plain WS through proxy
        // Most Discord WebSockets use wss:// anyway, so this path is rarely used
        match tokio::time::timeout(Duration::from_secs(10), connect_async(target_url)).await {
            Ok(Ok((ws, _resp))) => {
                let (sink, stream) = ws.split();
                (Arc::new(tokio::sync::Mutex::new(Box::pin(sink) as WsSinkDyn)), Box::pin(stream) as WsStreamDyn)
            },
            Ok(Err(e)) => {
                logger::error(&format!("Failed to connect to target WebSocket {}: {}", target_url, e), "ProxyServer", None).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
                return Err(anyhow!("WebSocket connection failed"));
            },
            Err(_) => {
                logger::error(&format!("Timeout connecting to target WebSocket: {}", target_url), "ProxyServer", None).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
                return Err(anyhow!("WebSocket connection timeout"));
            }
        }
    };

    logger::info(
        "WebSocket bridge established successfully",
        "ProxyServer",
        None
    ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));

    // Pipe messages in both directions
    let (cli_tx_raw, cli_rx_raw) = client_ws.split();
    let cli_tx: Arc<tokio::sync::Mutex<WsSinkDyn>> = Arc::new(tokio::sync::Mutex::new(Box::pin(cli_tx_raw)));
    let mut cli_rx: WsStreamDyn = Box::pin(cli_rx_raw);

    // Keepalive ping generator: send Ping messages over a channel to avoid borrowing rem_tx
    let (ping_tx, mut ping_rx) = tokio::sync::mpsc::unbounded_channel::<WsMessage>();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(25));
        loop {
            interval.tick().await;
            // ignore send errors if receiver dropped
            let _ = ping_tx.send(WsMessage::Ping(Vec::new().into()));
        }
    });

    let rem_tx_for_client = rem_tx.clone();
    let cli_tx_for_client = cli_tx.clone();
    let client_to_remote = async move {
        loop {
            tokio::select! {
                Some(msg_res) = cli_rx.next() => {
                    match msg_res {
                        Ok(msg) => {
                            // Respond to ping locally to keep the connection alive
                            if msg.is_ping() {
                                logger::debug("Received ping from client, sending pong", "ProxyServer", None).ok();
                                if let Err(e) = {
                                    let mut tx = cli_tx_for_client.lock().await;
                                    tx.send(WsMessage::Pong(msg.into_data().into())).await
                                } {
                                    logger::warning(&format!("Failed to send PONG to client: {}", e), "ProxyServer", None).ok();
                                    break;
                                }
                                continue;
                            }
                            // Forward close frames and then break
                            if msg.is_close() {
                                logger::info("Received close frame from client, forwarding to remote", "ProxyServer", None).ok();
                                let _ = {
                                    let mut tx = rem_tx_for_client.lock().await;
                                    tx.send(msg).await
                                };
                                break;
                            }
                            // Log message types for debugging
                            let msg_type = if msg.is_text() { "text" } else if msg.is_binary() { "binary" } else { "other" };
                            logger::debug(&format!("Forwarding {} message from client to remote", msg_type), "ProxyServer", None).ok();
                            
                            // Forward other messages to remote
                            if let Err(e) = {
                                let mut tx = rem_tx_for_client.lock().await;
                                tx.send(msg).await
                            } {
                                logger::error(&format!("Error forwarding message to remote: {}", e), "ProxyServer", None).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
                                break;
                            }
                        }
                        Err(e) => {
                            logger::error(&format!("Client->Remote websocket read error: {}", e), "ProxyServer", None).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
                            break;
                        }
                    }
                },
                Some(ping_msg) = ping_rx.recv() => {
                    if let Err(e) = {
                        let mut tx = rem_tx_for_client.lock().await;
                        tx.send(ping_msg).await
                    } {
                        logger::debug(&format!("Keepalive ping failed: {}", e), "ProxyServer", None).ok();
                        break;
                    }
                }
            }
        }
        anyhow::Ok(())
    };
    let rem_tx_for_remote = rem_tx.clone();
    let cli_tx_for_remote = cli_tx.clone();
    let state_for_emit = state.clone();
    let remote_to_client = async move {
        // Persistent streaming zlib decompressor for Discord Gateway (Z_SYNC_FLUSH per message)
        // Keep a single Decompress across frames; append produced bytes to msg_buf until flush marker
        let mut gw_decomp = Decompress::new(true); // true -> zlib headers
        let mut msg_buf: Vec<u8> = Vec::new();     // holds decompressed bytes for the current gateway message
    let mut _total_decompressed: u64 = 0;       // running counter to help with logging deltas (reserved for future diagnostics)
        while let Some(msg_res) = rem_rx.next().await {
            match msg_res {
                Ok(msg) => {
                    // Reply to ping from remote and do not forward ping to client
                    if msg.is_ping() {
                        logger::debug("Received ping from remote, sending pong", "ProxyServer", None).ok();
                        if let Err(e) = {
                            let mut tx = rem_tx_for_remote.lock().await;
                            tx.send(WsMessage::Pong(msg.into_data().into())).await
                        } {
                            logger::warning(&format!("Failed to send PONG to remote: {}", e), "ProxyServer", None).ok();
                            break;
                        }
                        continue;
                    }
                    // Forward close frames and then break
                    if msg.is_close() {
                        let _ = {
                            let mut tx = cli_tx_for_remote.lock().await;
                            tx.send(msg).await
                        };
                        break;
                    }

                    // If this is a Discord Gateway frame (text or compressed binary), try parse and emit minimal event
                    // Helper to show a native notification for MESSAGE_CREATE without duplicating code
                    let notify_message_create = |val: &serde_json::Value| {
                        // Extract owned data up-front so the returned future doesn't borrow `val`
                        let author_name: String = val
                            .get("d")
                            .and_then(|d| d.get("author"))
                            .and_then(|a| a.get("username"))
                            .and_then(|u| u.as_str())
                            .unwrap_or("Unknown")
                            .to_string();
                        let content_owned: String = val
                            .get("d")
                            .and_then(|d| d.get("content"))
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string();
                        let body_snippet: String = content_owned.chars().take(160).collect();
                        let title: String = format!("New message from {}", author_name);
                        let state_for_emit = state_for_emit.clone();
                        async move {
                            // 1) Show system notification
                            if let Some(handle) = state_for_emit
                                .app_handle
                                .lock()
                                .ok()
                                .and_then(|g| g.as_ref().cloned())
                            {
                                let _ = show_notification(handle.clone(), title.clone(), body_snippet.clone(), None).await;

                                // 2) Increment unread count and update badge
                                let mut unread = state_for_emit.unread_count.lock().await;
                                *unread = unread.saturating_add(1);
                                let count = *unread;
                                // Try to set the badge count on the main window
                                if let Some(win) = handle.get_webview_window("main").or_else(|| handle.get_webview_window("FreeDiscord")).or_else(|| handle.webview_windows().values().next().cloned()) {
                                    let _ = win.set_badge_count(Some(count as i64));
                                    // 3) Flash the window for attention
                                    let _ = win.request_user_attention(Some(tauri::UserAttentionType::Critical));
                                }
                            }
                        }
                    };
                    if msg.is_text() || msg.is_binary() {
                        let frame_len = msg.clone().into_data().len();
                        let frame_kind = if msg.is_text() { "TEXT" } else { "BINARY" };
                        logger::debug(&format!("Gateway {} frame len={}", frame_kind, frame_len), "ProxyServer", None).ok();

                        // Handle TEXT frames directly
                        if msg.is_text() {
                            let bytes = msg.clone().into_data();
                            match std::str::from_utf8(&bytes) {
                                Ok(txt) => {
                                    let excerpt: String = txt.chars().take(180).collect();
                                    match serde_json::from_str::<serde_json::Value>(txt) {
                                        Ok(val) => {
                                            let op_val = val.get("op").and_then(|v| v.as_i64());
                                            let event_type = val.get("t").and_then(|v| v.as_str()).unwrap_or("");
                                            logger::debug(&format!("Parsed Discord Gateway event: {} (op={:?})", if event_type.is_empty() { "<none>" } else { event_type }, op_val), "ProxyServer", None).ok();
                                            if event_type == "MESSAGE_CREATE" { notify_message_create(&val).await; }
                                        }
                                        Err(e) => {
                                            logger::debug(&format!("Gateway frame parse failed: JSON parse error (text): {} | excerpt='{}'", e, excerpt), "ProxyServer", None).ok();
                                        }
                                    }
                                }
                                Err(e) => {
                                    logger::debug(&format!("Gateway frame parse failed: UTF-8 error (text): {}", e), "ProxyServer", None).ok();
                                }
                            }
                        }

                        // Handle BINARY frames via a persistent zlib stream; parse on Z_SYNC_FLUSH marker (00 00 ff ff)
                        if msg.is_binary() {
                            let data = msg.clone().into_data();
                            logger::debug(&format!("Gateway BINARY frame len={}", data.len()), "ProxyServer", None).ok();

                            // Feed the chunk to the decompressor in a loop until fully consumed
                            let mut input: &[u8] = &data;
                            let mut newly_produced_total: usize = 0;
                            while !input.is_empty() {
                                // Working output buffer for this iteration
                                let mut out_tmp = vec![0u8; 64 * 1024];
                                let in_before = gw_decomp.total_in();
                                let out_before = gw_decomp.total_out();
                                match gw_decomp.decompress(input, &mut out_tmp, FlushDecompress::Sync) {
                                    Ok(_status) => {
                                        let in_after = gw_decomp.total_in();
                                        let out_after = gw_decomp.total_out();
                                        let consumed = (in_after - in_before) as usize;
                                        let produced = (out_after - out_before) as usize;
                                        if produced > 0 {
                                            newly_produced_total += produced;
                                            msg_buf.extend_from_slice(&out_tmp[..produced]);
                                        }
                                        if consumed == 0 {
                                            // Avoid infinite loop on no-progress; break out
                                            break;
                                        }
                                        input = &input[consumed..];
                                    }
                                    Err(e) => {
                                        // If the stream got out of sync (e.g., we started mid-stream), try resetting once
                                        let sample_len = data.len().min(24);
                                        let hex_sample: String = data[..sample_len].iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ");
                                        logger::debug(&format!("Gateway frame parse failed: streaming zlib decode error: {} | first {} bytes: {}", e, sample_len, hex_sample), "ProxyServer", None).ok();
                                        // Reset decompressor and current message buffer to recover
                                        gw_decomp = Decompress::new(true);
                                        msg_buf.clear();
                                        break;
                                    }
                                }
                            }

                            // If this frame ends with Z_SYNC_FLUSH marker, attempt to parse the accumulated message
                            if data.len() >= 4 && &data[data.len()-4..] == [0x00, 0x00, 0xff, 0xff] {
                                if !msg_buf.is_empty() {
                                    match String::from_utf8(msg_buf.clone()) {
                                        Ok(s) => {
                                            let excerpt: String = s.chars().take(180).collect();
                                            match serde_json::from_str::<serde_json::Value>(&s) {
                                                Ok(val) => {
                                                    let op_val = val.get("op").and_then(|v| v.as_i64());
                                                    let event_type = val.get("t").and_then(|v| v.as_str()).unwrap_or("");
                                                    logger::debug(&format!("Parsed Discord Gateway event (zlib stream): {} (op={:?})", if event_type.is_empty() { "<none>" } else { event_type }, op_val), "ProxyServer", None).ok();
                                                    if event_type == "MESSAGE_CREATE" { notify_message_create(&val).await; }
                                                }
                                                Err(e) => {
                                                    logger::debug(&format!("Gateway frame parse failed: JSON parse error (binary zlib stream): {} | excerpt='{}'", e, excerpt), "ProxyServer", None).ok();
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            logger::debug(&format!("Gateway binary produced non-UTF8 after zlib stream: {} ({} bytes)", e, msg_buf.len()), "ProxyServer", None).ok();
                                        }
                                    }
                                }
                                // Clear per-message buffer on flush boundary (stream continues)
                                _total_decompressed = gw_decomp.total_out();
                                msg_buf.clear();
                            } else if newly_produced_total > 0 {
                                // Optional: log partial progress for long messages
                                logger::debug(&format!("Gateway zlib stream produced +{} bytes (total_out={})", newly_produced_total, gw_decomp.total_out()), "ProxyServer", None).ok();
                            }
                        }
                    }

                    if let Err(e) = {
                        let mut tx = cli_tx_for_remote.lock().await;
                        tx.send(msg).await
                    } {
                        logger::warning(&format!("Error forwarding message to client: {}", e), "ProxyServer", None).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
                        break;
                    }
                }
                Err(e) => {
                    logger::warning(&format!("Remote->Client websocket read error: {}", e), "ProxyServer", None).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
                    break;
                }
            }
        }
        anyhow::Ok(())
    };

    // Run both directions concurrently
    tokio::select! {
        res = client_to_remote => { 
            if let Err(e) = res {
                logger::warning(
                    &format!("Client to remote bridge ended with error: {}", e),
                    "ProxyServer",
                    None
                ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
            }
        },
        res = remote_to_client => { 
            if let Err(e) = res {
                logger::warning(
                    &format!("Remote to client bridge ended with error: {}", e),
                    "ProxyServer",
                    None
                ).unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
            }
        }
    }

    logger::info(
        "WebSocket bridge closed",
        "ProxyServer",
        None
    ).unwrap_or_else(|e| eprintln!("Failed to log: {}", e));

    Ok(())
}

// get_proxies_paginated: return current page only (no auto testing here)
#[tauri::command]
async fn get_proxies_paginated(
    _window: Window,
    state: State<'_, Arc<AppState>>,
    page: usize,
    page_size: usize
) -> Result<PaginatedProxies, String> {
    let proxies_lock = state.proxies.lock().await;
    let testing_queue = state.proxy_testing_queue.lock().await;
    
    let total_count = proxies_lock.len();
    let total_pages = (total_count + page_size - 1) / page_size;
    let current_page = page.min(total_pages.max(1) - 1);
    
    let start = current_page * page_size;
    let end = (start + page_size).min(total_count);
    
    let mut page_proxies = if start < total_count {
        proxies_lock[start..end].to_vec()
    } else {
        Vec::new()
    };
    
    // Update status of each proxy based on testing queue
    for proxy in &mut page_proxies {
        let proxy_key = format!("{}:{}", proxy.ip, proxy.port);
        if testing_queue.contains(&proxy_key) {
            proxy.status = "testing".to_string();
        } else if proxy.ping_ms >= 0 {
            proxy.status = if proxy.is_working { "tested".to_string() } else { "failed".to_string() };
        }
    }
    
    // No background tests here. Explicit Step 3 will handle testing when desired.
    
    // Return the page with current status
    Ok(PaginatedProxies {
        proxies: page_proxies,
        total_count,
        total_pages,
        current_page,
    })
}

// Command to test custom proxy connectivity with detailed diagnostics
#[tauri::command]
async fn test_custom_proxy_detailed(
    ip: String,
    port: u16,
    proxy_type: String,
    username: Option<String>,
    password: Option<String>,
) -> Result<String, String> {
    logger::info(&format!("Starting detailed test for custom proxy {}:{} ({})", ip, port, proxy_type), "ProxyTester", None)
        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
    
    let mut results = Vec::new();
    
    // Test 1: Basic TCP connectivity
    results.push("=== BASIC CONNECTIVITY TEST ===".to_string());
    let addr = format!("{}:{}", ip, port);
    match addr.parse::<std::net::SocketAddr>() {
        Ok(socket_addr) => {
            match tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(socket_addr)).await {
                Ok(Ok(_)) => {
                    results.push("✓ TCP connection successful".to_string());
                },
                Ok(Err(e)) => {
                    results.push(format!("✗ TCP connection failed: {}", e));
                    return Ok(results.join("\n"));
                },
                Err(_) => {
                    results.push("✗ TCP connection timeout (5s)".to_string());
                    return Ok(results.join("\n"));
                }
            }
        },
        Err(e) => {
            results.push(format!("✗ Invalid address format: {}", e));
            return Ok(results.join("\n"));
        }
    }
    
    // Test 2: Proxy protocol test
    results.push("\n=== PROXY PROTOCOL TEST ===".to_string());
    let client = match proxy_type.to_lowercase().as_str() {
        "socks4" | "socks5" => {
            // Embed credentials into the SOCKS URL if provided
            let proxy_url = if let (Some(u), Some(p)) = (username.as_ref(), password.as_ref()) {
                format!("socks5://{}:{}@{}:{}", u, p, ip, port)
            } else {
                format!("socks5://{}:{}", ip, port)
            };
            match reqwest::Proxy::all(&proxy_url) {
                Ok(proxy) => {
                    match reqwest::Client::builder()
                        .proxy(proxy)
                        .timeout(Duration::from_secs(6))
                        .danger_accept_invalid_certs(true)
                        .build() {
                        Ok(client) => {
                            results.push("✓ SOCKS proxy client created".to_string());
                            client
                        },
                        Err(e) => {
                            results.push(format!("✗ Failed to create SOCKS client: {}", e));
                            return Ok(results.join("\n"));
                        }
                    }
                },
                Err(e) => {
                    results.push(format!("✗ Failed to create SOCKS proxy config: {}", e));
                    return Ok(results.join("\n"));
                }
            }
        },
        _ => {
            // HTTP/HTTPS proxy
            let base_url = format!("http://{}:{}", ip, port);
            let mut proxy = match reqwest::Proxy::all(&base_url) {
                Ok(p) => p,
                Err(e) => {
                    results.push(format!("✗ Failed to create HTTP proxy config: {}", e));
                    return Ok(results.join("\n"));
                }
            };
            if let (Some(u), Some(p)) = (username.as_ref(), password.as_ref()) {
                proxy = proxy.basic_auth(u, p);
                results.push("✓ Using proxy authentication".to_string());
            }
            match reqwest::Client::builder()
                .proxy(proxy)
                .timeout(Duration::from_secs(6))
                .danger_accept_invalid_certs(true)
                .build() {
                Ok(client) => {
                    results.push("✓ HTTP proxy client created".to_string());
                    client
                },
                Err(e) => {
                    results.push(format!("✗ Failed to create HTTP client: {}", e));
                    return Ok(results.join("\n"));
                }
            }
        }
    };
    
    // Tests 3-6: Run in parallel with short timeouts to reduce total duration
    results.push("\n=== PARALLEL CONNECTIVITY TESTS ===".to_string());
    let client_arc = Arc::new(client);
    let c1 = client_arc.clone();
    let c2 = client_arc.clone();
    let c3 = client_arc.clone();
    let c4 = client_arc.clone();

    let t_httpbin = tokio::spawn(async move {
        match tokio::time::timeout(Duration::from_secs(5), c1.get("https://httpbin.org/ip").send()).await {
            Ok(Ok(resp)) => Some(resp),
            _ => None,
        }
    });
    let t_cdn = tokio::spawn(async move {
        match tokio::time::timeout(Duration::from_secs(5), c2.get("https://cdn.discordapp.com/attachments/1/1/1.txt").send()).await {
            Ok(Ok(resp)) => Some(resp),
            _ => None,
        }
    });
    let t_api = tokio::spawn(async move {
        match tokio::time::timeout(Duration::from_secs(5), c3.get("https://discord.com/api/v9/gateway").send()).await {
            Ok(Ok(resp)) => Some(resp),
            _ => None,
        }
    });
    let t_main = tokio::spawn(async move {
        match tokio::time::timeout(Duration::from_secs(5), c4.get("https://discord.com/").send()).await {
            Ok(Ok(resp)) => Some(resp),
            _ => None,
        }
    });

    let (r_httpbin, r_cdn, r_api, r_main) = tokio::join!(t_httpbin, t_cdn, t_api, t_main);

    results.push("\n=== INTERNET CONNECTIVITY TEST ===".to_string());
    match r_httpbin {
        Ok(Some(resp)) => {
            if resp.status().is_success() {
                results.push("✓ Internet access confirmed".to_string());
            } else {
                results.push(format!("✗ Internet test failed with status: {}", resp.status()));
            }
        }
        Ok(None) => results.push("✗ Internet test timed out or failed".to_string()),
        Err(e) => results.push(format!("✗ Internet test join error: {}", e)),
    }

    results.push("\n=== DISCORD CDN TEST ===".to_string());
    match r_cdn {
        Ok(Some(resp)) => {
            let status = resp.status();
            if status.is_success() || status == reqwest::StatusCode::NOT_FOUND || status.is_redirection() {
                results.push("✓ Discord CDN accessible".to_string());
            } else {
                results.push(format!("✗ Discord CDN returned status: {}", status));
            }
        }
        Ok(None) => results.push("✗ Discord CDN test timed out or failed".to_string()),
        Err(e) => results.push(format!("✗ Discord CDN test join error: {}", e)),
    }

    results.push("\n=== DISCORD API TEST ===".to_string());
    match r_api {
        Ok(Some(resp)) => {
            if resp.status().is_success() || resp.status().as_u16() == 404 {
                results.push("✓ Discord API reachable".to_string());
            } else {
                results.push(format!("✗ Discord API returned status: {}", resp.status()));
            }
        }
        Ok(None) => results.push("✗ Discord API test timed out or failed".to_string()),
        Err(e) => results.push(format!("✗ Discord API test join error: {}", e)),
    }

    results.push("\n=== DISCORD MAIN SITE TEST ===".to_string());
    match r_main {
        Ok(Some(resp)) => {
            if resp.status().is_success() || resp.status().is_redirection() {
                results.push("✓ Discord main site accessible".to_string());
            } else {
                results.push(format!("✗ Discord main site returned status: {}", resp.status()));
            }
        }
        Ok(None) => results.push("✗ Discord main site test timed out or failed".to_string()),
        Err(e) => results.push(format!("✗ Discord main site test join error: {}", e)),
    }
    
    results.push("\n=== TEST COMPLETE ===".to_string());
    Ok(results.join("\n"))
}

// Helper function get_proxies_needing_test removed (unused)

// Modify the test_proxy command to use the internal function
#[tauri::command]
async fn test_proxy(
    state: State<'_, Arc<AppState>>, 
    ip: String, 
    port: u16
) -> Result<ProxyInfo, String> {
    test_proxy_internal(&ip, port, &state.inner()).await
}

// Command to add a custom proxy provided by the user
#[tauri::command]
async fn add_custom_proxy(
    window: Window,
    state: State<'_, Arc<AppState>>,
    ip: String,
    port: u16,
    proxy_type: Option<String>,
    username: Option<String>,
    password: Option<String>,
) -> Result<ProxyInfo, String> {
    // Validate IP
    if !is_valid_ip(&ip) {
        return Err(format!("Invalid IP address: {}", ip));
    }

    if port == 0 {
        return Err(format!("Invalid port: {}", port));
    }

    let ptype = proxy_type.unwrap_or_else(|| "http".to_string());

    let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();

    let proxy = ProxyInfo {
        ip: ip.clone(),
        port,
        country: "Unknown".to_string(),
        city: "Unknown".to_string(),
        isp: "Unknown".to_string(),
        ping_ms: -1,
        proxy_type: ptype.clone(),
        last_checked: now.clone(),
        is_working: false,
        status: "pending".to_string(),
        username,
        password,
    };

    // Insert into state (deduplicate)
    {
        let mut proxies = state.proxies.lock().await;
        let key = format!("{}:{}", ip, port);
        if proxies.iter().any(|p| format!("{}:{}", p.ip, p.port) == key) {
            // Already present - return existing
            if let Some(existing) = proxies.iter().find(|p| p.ip == ip && p.port == port) {
                return Ok(existing.clone());
            }
        }

        proxies.push(proxy.clone());
    }

    // Notify frontend that proxies updated
    let _ = window.emit("proxies-updated", true);

    // Spawn background test for the newly added proxy
    let state_clone = state.inner().clone();
    let window_clone = window.clone();
    tokio::spawn(async move {
        match test_proxy_internal(&ip, port, &state_clone).await {
            Ok(tested) => {
                let _ = window_clone.emit("proxy-tested", tested.clone());
                // If working, save cache (fire-and-forget)
                if tested.is_working {
                    let _ = save_proxies_to_cache(&state_clone).await;
                }
            }
            Err(e) => {
                let log = format!("Custom proxy test failed for {}:{} -> {}", ip, port, e);
                emit_log(Some(&window_clone), &log);
            }
        }
    });

    Ok(proxy)
}

// Command to delete a proxy from the list
#[tauri::command]
async fn delete_proxy(
    window: Window,
    state: State<'_, Arc<AppState>>,
    ip: String,
    port: u16,
) -> Result<String, String> {
    // Remove from state and compute if anything was removed
    let removed = {
        let mut proxies = state.proxies.lock().await;
        let initial_len = proxies.len();
        proxies.retain(|p| !(p.ip == ip && p.port == port));
        proxies.len() < initial_len
    };
    
    if removed {
        // Check if the deleted proxy was the active proxy
        {
            let mut active_proxy = state.active_proxy.lock().await;
            if let Some(ref active) = *active_proxy {
                if active.ip == ip && active.port == port {
                    // Stop the proxy if it was running
                    let mut is_running = state.is_proxy_running.lock().await;
                    *is_running = false;
                    *active_proxy = None;
                    
                    let mut port_lock = state.current_proxy_port.lock().await;
                    *port_lock = None;
                    
                    logger::info(&format!("Stopped active proxy {}:{} because it was deleted", ip, port), "ProxyManager", None)
                        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
                }
            }
        }
        
        // Notify frontend that proxies updated
        let _ = window.emit("proxies-updated", true);
        
        logger::info(&format!("Deleted proxy {}:{}", ip, port), "ProxyManager", None)
            .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
        
        Ok(format!("Proxy {}:{} deleted successfully", ip, port))
    } else {
        Err(format!("Proxy {}:{} not found", ip, port))
    }
}

// Command to clear the proxies cache
#[tauri::command]
async fn clear_proxies_cache(
    window: Window,
    state: State<'_, Arc<AppState>>,
    webview: Webview,
) -> Result<String, String> {
    // Clear the in-memory proxy list
    {
        let mut proxies = state.proxies.lock().await;
        let count = proxies.len();
        proxies.clear();
        
        logger::info(&format!("Cleared {} proxies from memory", count), "CacheManager", None)
            .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
    }
    
    // Stop any active proxy if running
    {
        let mut is_running = state.is_proxy_running.lock().await;
        if *is_running {
            *is_running = false;
            
            let mut active_proxy = state.active_proxy.lock().await;
            *active_proxy = None;
            
            let mut port_lock = state.current_proxy_port.lock().await;
            *port_lock = None;
            
            logger::info("Stopped active proxy due to cache clear", "CacheManager", None)
                .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
        }
    }
    
    // Try to delete the cache file if it exists (use app data dir helper)
    let cache_result = {
        let cache_file = get_proxy_cache_file();
        if cache_file.exists() {
            match std::fs::remove_file(&cache_file) {
                Ok(_) => {
                    logger::info("Deleted proxies cache file", "CacheManager", None)
                        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
                    "Cache file deleted"
                },
                Err(e) => {
                    logger::warning(&format!("Failed to delete cache file: {}", e), "CacheManager", None)
                        .unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err));
                    "Cache cleared but file deletion failed"
                }
            }
        } else {
            "Cache cleared (no file to delete)"
        }
    };

    // Clear webview browsing data (cache/cookies/storage) to ensure a clean slate
    match webview.clear_all_browsing_data() {
        Ok(_) => logger::info("Cleared webview browsing data", "CacheManager", None)
            .unwrap_or_else(|e| eprintln!("Failed to log: {}", e)),
        Err(e) => logger::warning(&format!("Failed to clear webview data: {}", e), "CacheManager", None)
            .unwrap_or_else(|log_err| eprintln!("Failed to log: {}", log_err)),
    }
    
    // Notify frontend that proxies were cleared
    let _ = window.emit("proxies-updated", true);
    
    Ok(format!("Cache cleared successfully. {}", cache_result))
}

#[tauri::command]
async fn get_proxy_testing_status(state: State<'_, Arc<AppState>>) -> Result<(usize, bool), String> {
    let is_fetching = {
        let fetching = state.fetching_in_progress.lock().await;
        *fetching
    };
    
    let testing_count = {
        let queue = state.proxy_testing_queue.lock().await;
        queue.len()
    };
    
    Ok((testing_count, is_fetching))
}

// Get the proxy cache file path
fn get_proxy_cache_file() -> PathBuf {
    get_app_data_dir().join("proxies_cache.json")
}

// Save proxies to a cache file
async fn save_proxies_to_cache(state: &Arc<AppState>) -> Result<(), String> {
    let cache_file = get_proxy_cache_file();
    
    // Get only working proxies for cache
    let working_proxies = {
        let proxies = state.proxies.lock().await;
        proxies.iter()
            .filter(|p| p.is_working)
            .cloned()
            .collect::<Vec<ProxyInfo>>()
    };
    
    if working_proxies.is_empty() {
        // No need to save if there are no working proxies
        return Ok(());
    }
    
    // Serialize to JSON
    let json = match serde_json::to_string_pretty(&working_proxies) {
        Ok(j) => j,
        Err(e) => return Err(format!("Failed to serialize proxies: {}", e)),
    };
    
    // Save to file
    handle_log_error(
        logger::info(&format!("Saving {} working proxies to cache", working_proxies.len()), 
                    "ProxyCache", None),
        "Failed to log cache save operation"
    );
    
    match fs::write(&cache_file, json) {
        Ok(_) => Ok(()),
        Err(e) => {
            handle_log_error(
                logger::error(&format!("Failed to write proxy cache file: {}", e), 
                         "ProxyCache", None),
                "Failed to log cache write error"
            );
            Err(format!("Failed to write cache file: {}", e))
        }
    }
}

// Load proxies from cache file
async fn load_proxies_from_cache(state: &Arc<AppState>) -> Result<(), String> {
    let cache_file = get_proxy_cache_file();
    
    // Check if cache file exists
    if !cache_file.exists() {
        return Ok(());
    }
    
    // Read file content
    let content = match fs::read_to_string(&cache_file) {
        Ok(c) => c,
        Err(e) => {
            handle_log_error(
                logger::warning(&format!("Failed to read proxy cache file: {}", e), 
                            "ProxyCache", None),
                "Failed to log cache read error"
            );
            return Err(format!("Failed to read cache file: {}", e));
        }
    };
    
    // Deserialize JSON
    let cached_proxies: Vec<ProxyInfo> = match serde_json::from_str(&content) {
        Ok(p) => p,
        Err(e) => {
            handle_log_error(
                logger::warning(&format!("Failed to parse proxy cache file: {}", e), 
                            "ProxyCache", None),
                "Failed to log cache parse error"
            );
            return Err(format!("Failed to parse cache file: {}", e));
        }
    };
    
    if !cached_proxies.is_empty() {
        handle_log_error(
            logger::info(&format!("Loaded {} proxies from cache", cached_proxies.len()), 
                       "ProxyCache", None),
            "Failed to log cache load operation"
        );
        
        // Update state with cached proxies
        let mut proxies_lock = state.proxies.lock().await;
        *proxies_lock = cached_proxies;
    }
    
    Ok(())
}

// Command to save proxies to cache
#[tauri::command]
async fn save_proxies_cache(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    match save_proxies_to_cache(&state.inner()).await {
        Ok(_) => Ok("Successfully saved proxies to cache".to_string()),
        Err(e) => Err(e),
    }
}

// Command to load proxies from cache
#[tauri::command]
async fn load_proxies_cache(window: Window, state: State<'_, Arc<AppState>>) -> Result<String, String> {
    match load_proxies_from_cache(&state.inner()).await {
        Ok(_) => {
            // Notify frontend about the updated proxies
            let _ = window.emit("proxies-updated", true);
            Ok("Successfully loaded proxies from cache".to_string())
        },
        Err(e) => Err(e),
    }
}

// Command to navigate the main window to a URL
#[tauri::command]
async fn navigate_to_url(window: Window, url: String) -> Result<String, String> {
    logger::info(&format!("Navigating main window to URL: {}", url), "Navigation", None)
        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
    
    // Validate URL
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("Invalid URL format. Must start with http:// or https://".to_string());
    }
    
    // Emit an event to the JS side to handle navigation
    if let Err(e) = window.emit("navigate-main-window", url.clone()) {
        return Err(format!("Failed to emit navigation event: {}", e));
    }
    
    Ok(format!("Navigating to {}", url))
}

// Command to open a URL in the system default browser (outside the app)
#[tauri::command]
async fn open_in_system_browser(url: String) -> Result<String, String> {
    logger::info(&format!("Opening in system browser: {}", url), "SystemBrowser", None)
        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));

    // Basic validation
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("Invalid URL format. Must start with http:// or https://".to_string());
    }

    // Use the `open` crate to open the URL with the OS default handler
    match open_in_browser::that(url.clone()) {
        Ok(_) => Ok(format!("Opened {} in system browser", url)),
        Err(e) => Err(format!("Failed to open browser: {}", e)),
    }
}

// Command to show notification
#[tauri::command]
async fn show_notification(
    _app: tauri::AppHandle,
    title: String,
    body: String,
    icon: Option<String>
) -> Result<String, String> {
    let title_for_log = title.clone();
    logger::info(&format!("[DEBUG] show_notification called with title: '{}', body: '{}'", title_for_log, body), "Notifications", None)
        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));

    match tokio::task::spawn_blocking(move || {
        eprintln!("[DEBUG] Creating notification...");

        let mut notification = Notification::new();

        eprintln!("[DEBUG] Setting notification properties...");
        notification
            .summary(&title)
            .body(&body)
            .timeout(5000) // 5 second timeout
            .appname("FreeDiscord");

        // Set icon if provided
        if let Some(icon_path) = &icon {
            eprintln!("[DEBUG] Setting icon: {}", icon_path);
            notification.icon(icon_path);
        }

        eprintln!("[DEBUG] Attempting to show notification...");
        let result = notification.show();
        eprintln!("[DEBUG] Notification show result: {:?}", result);
        result
    }).await {
        Ok(result) => match result {
            Ok(handle) => {
                logger::info(&format!("[DEBUG] Notification shown successfully: {} (handle: {:?})", title_for_log, handle), "Notifications", None)
                    .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
                Ok("Notification shown successfully".to_string())
            }
            Err(e) => {
                logger::error(&format!("[DEBUG] Failed to show notification: {}", e), "Notifications", None)
                    .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
                eprintln!("[DEBUG] Notification error details: {:?}", e);
                Err(format!("Failed to show notification: {}", e))
            }
        },
        Err(e) => {
            logger::error(&format!("[DEBUG] Failed to spawn notification task: {}", e), "Notifications", None)
                .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
            eprintln!("[DEBUG] Spawn error details: {:?}", e);
            Err(format!("Failed to spawn notification task: {}", e))
        }
    }
}

// Command to set badge count on taskbar (Windows) or dock (macOS)
#[tauri::command]
async fn set_badge_count(app: tauri::AppHandle, count: Option<u32>) -> Result<String, String> {
    eprintln!("[DEBUG] set_badge_count called with count: {:?}", count);

    // Try to get window by different possible labels
    let window = app.get_webview_window("main")
        .or_else(|| app.get_webview_window("FreeDiscord"))
        .or_else(|| {
            // Get any available window
            let windows = app.webview_windows();
            eprintln!("[DEBUG] Available windows: {:?}", windows.keys().collect::<Vec<_>>());
            windows.values().next().cloned()
        });

    match window {
        Some(window) => {
            eprintln!("[DEBUG] Found window, setting badge count...");
            if let Some(count_val) = count {
                if count_val > 0 {
                    logger::debug(&format!("[DEBUG] Setting badge count to: {}", count_val), "Notifications", None)
                        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
                    eprintln!("[DEBUG] Calling set_badge_count with: {}", count_val);
                    match window.set_badge_count(Some(count_val as i64)) {
                        Ok(_) => {
                            eprintln!("[DEBUG] Badge count set successfully");
                            Ok("Badge count updated successfully".to_string())
                        }
                        Err(e) => {
                            eprintln!("[DEBUG] Failed to set badge count: {:?}", e);
                            Err(format!("Failed to set badge count: {}", e))
                        }
                    }
                } else {
                    logger::debug("[DEBUG] Clearing badge count (count is 0)", "Notifications", None)
                        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
                    eprintln!("[DEBUG] Calling set_badge_count with None (clearing)");
                    match window.set_badge_count(None) {
                        Ok(_) => {
                            eprintln!("[DEBUG] Badge count cleared successfully");
                            Ok("Badge count cleared successfully".to_string())
                        }
                        Err(e) => {
                            eprintln!("[DEBUG] Failed to clear badge count: {:?}", e);
                            Err(format!("Failed to clear badge count: {}", e))
                        }
                    }
                }
            } else {
                logger::debug("[DEBUG] Clearing badge count (count is None)", "Notifications", None)
                    .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
                eprintln!("[DEBUG] Calling set_badge_count with None (clearing)");
                match window.set_badge_count(None) {
                    Ok(_) => {
                        eprintln!("[DEBUG] Badge count cleared successfully");
                        Ok("Badge count cleared successfully".to_string())
                    }
                    Err(e) => {
                        eprintln!("[DEBUG] Failed to clear badge count: {:?}", e);
                        Err(format!("Failed to clear badge count: {}", e))
                    }
                }
            }
        }
        None => {
            logger::error("[DEBUG] No window found for badge count", "Notifications", None)
                .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
            eprintln!("[DEBUG] No window found - available windows: {:?}", app.webview_windows().keys().collect::<Vec<_>>());
            Err("No window found".to_string())
        }
    }
}

// Command to flash window for attention
#[tauri::command]
async fn flash_window(app: tauri::AppHandle) -> Result<String, String> {
    eprintln!("[DEBUG] flash_window called");
    logger::debug("[DEBUG] Flashing window for attention", "Notifications", None)
        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));

    // Try to get window by different possible labels
    let window = app.get_webview_window("main")
        .or_else(|| app.get_webview_window("FreeDiscord"))
        .or_else(|| {
            // Get any available window
            let windows = app.webview_windows();
            eprintln!("[DEBUG] Available windows for flashing: {:?}", windows.keys().collect::<Vec<_>>());
            windows.values().next().cloned()
        });

    match window {
        Some(window) => {
            eprintln!("[DEBUG] Found window, attempting to flash...");
            match window.request_user_attention(Some(tauri::UserAttentionType::Critical)) {
                Ok(_) => {
                    eprintln!("[DEBUG] Window flashed successfully");
                    Ok("Window flashed successfully".to_string())
                }
                Err(e) => {
                    eprintln!("[DEBUG] Failed to flash window: {:?}", e);
                    Err(format!("Failed to flash window: {}", e))
                }
            }
        }
        None => {
            logger::error("[DEBUG] No window found for flashing", "Notifications", None)
                .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
            eprintln!("[DEBUG] No window found for flashing - available windows: {:?}", app.webview_windows().keys().collect::<Vec<_>>());
            Err("No window found".to_string())
        }
    }
}

// Command to update tray icon with notification indicator
#[tauri::command]
async fn update_tray_notification(
    app: tauri::AppHandle,
    has_notifications: bool
) -> Result<String, String> {
    logger::debug(&format!("Updating tray notification indicator: {}", has_notifications), "Notifications", None)
        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));

    match app.tray_by_id("main") {
        Some(tray) => {
            let tooltip = if has_notifications {
                "FreeDiscord - Discord Proxy Client (New messages)"
            } else {
                "FreeDiscord - Discord Proxy Client"
            };

            tray.set_tooltip(Some(tooltip))
                .map_err(|e| format!("Failed to update tray tooltip: {}", e))?;

            Ok("Tray notification updated successfully".to_string())
        }
        None => {
            logger::error("No tray icon found", "Notifications", None)
                .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
            Err("No tray icon found".to_string())
        }
    }
}

// Helper function to show tray minimize notification
async fn show_tray_minimize_notification(_app: tauri::AppHandle) -> Result<(), String> {
    // Only show the hint once per session
    if TRAY_HINT_SHOWN.load(Ordering::Relaxed) {
        eprintln!("[DEBUG] Tray hint already shown this session, skipping");
        return Ok(());
    }

    eprintln!("[DEBUG] Showing tray minimize notification hint (first time this session)");

    // Mark as shown
    TRAY_HINT_SHOWN.store(true, Ordering::Relaxed);

    // Show a helpful notification when app is minimized to tray
    match tokio::task::spawn_blocking(move || {
        let mut notification = Notification::new();
        notification
            .summary("FreeDiscord Minimized")
            .body("App is now running in the system tray. Right-click the tray icon to restore the window or access options.")
            .timeout(7000) // 7 second timeout for more time to read
            .appname("FreeDiscord");

        notification.show()
    }).await {
        Ok(result) => match result {
            Ok(_) => {
                logger::info("[DEBUG] Tray minimize notification shown successfully", "Notifications", None)
                    .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
                eprintln!("[DEBUG] Tray minimize notification shown");
                Ok(())
            }
            Err(e) => {
                logger::warning(&format!("[DEBUG] Failed to show tray minimize notification: {}", e), "Notifications", None)
                    .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
                eprintln!("[DEBUG] Tray notification error: {:?}", e);
                // Reset the flag so it can be tried again
                TRAY_HINT_SHOWN.store(false, Ordering::Relaxed);
                // Don't return error as this is not critical
                Ok(())
            }
        },
        Err(e) => {
            logger::warning(&format!("[DEBUG] Failed to spawn tray notification task: {}", e), "Notifications", None)
                .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
            eprintln!("[DEBUG] Spawn error: {:?}", e);
            // Reset the flag so it can be tried again
            TRAY_HINT_SHOWN.store(false, Ordering::Relaxed);
            // Don't return error as this is not critical
            Ok(())
        }
    }
}

// Command to properly exit the application
#[tauri::command]
async fn exit_application() -> Result<String, String> {
    logger::info("Application exit requested via command", "system", None)
        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));

    // Give time for frontend cleanup
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    std::process::exit(0);
}

// Command to shutdown notifications
#[tauri::command]
async fn shutdown_notifications() -> Result<String, String> {
    logger::info("Notification shutdown requested", "Notifications", None)
        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));

    Ok("Notification shutdown signal sent".to_string())
}

// Command to return to the proxy manager UI
#[tauri::command]
async fn return_to_proxy_manager(window: Window) -> Result<String, String> {
    logger::info("Returning to proxy manager UI", "Navigation", None)
        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
    
    // Emit an event to the JS side to return to the main UI
    if let Err(e) = window.emit("return-to-proxy-manager", ()) {
        return Err(format!("Failed to emit return event: {}", e));
    }
    
    Ok("Returned to proxy manager".to_string())
}

#[tauri::command]
async fn clear_discord_session_data(webview: Webview) -> Result<String, String> {
    logger::info("Clearing Discord session data", "SessionManager", None)
        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));

    // Best-effort: remove cookies for discord.com and clear site storage
    // Note: On Windows, cookie APIs should be invoked from async commands (this one is async).
    let mut cleared_cookie_count: usize = 0;
    let discord_urls = [
        "https://discord.com",
        "https://www.discord.com",
        "https://gateway.discord.gg",
        "https://remote-auth-gateway.discord.gg",
    ];

    for u in discord_urls.iter() {
        if let Ok(url) = Url::parse(u) {
            match webview.cookies_for_url(url) {
                Ok(cookies) => {
                    for c in cookies {
                        if let Err(e) = webview.delete_cookie(c.clone()) {
                            logger::warning(&format!("Failed to delete cookie {}: {}", c.name(), e), "SessionManager", None)
                                .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
                        } else {
                            cleared_cookie_count += 1;
                        }
                    }
                }
                Err(e) => {
                    logger::warning(&format!("Failed to get cookies for {}: {}", u, e), "SessionManager", None)
                        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));
                }
            }
        }
    }

    // Clear local/session storage and any service worker cache for the current page context
    let _ = webview.eval(
        "try{localStorage.clear();sessionStorage.clear();if(window.caches){caches.keys().then(keys=>keys.forEach(k=>caches.delete(k))).catch(()=>{});} }catch(_){}",
    );

    logger::info(&format!("Cleared {} Discord cookies and site storage", cleared_cookie_count), "SessionManager", None)
        .unwrap_or_else(|e| eprintln!("Failed to log: {}", e));

    Ok("Discord session data cleared".to_string())
}

// Modify the inject_scripts_into_html function
fn inject_scripts_into_html(html_content: &str) -> String {
    // Create a simplified script that handles basic functionality with hCaptcha fixes
    let script = r#"
    <script>
    // Discord Proxy Helper with hCaptcha fixes
        (function() {
        console.log("[DC-PROXY] Loading client-side handlers with hCaptcha support");
        
        // Global error tracking
        window.addEventListener('error', function(event) {
            console.error('[DC-PROXY] Global Error:', event.message, 'at', event.filename, ':', event.lineno);
        });
        
        // FIXED: Don't attempt to redefine window.location to prevent TypeError
        // Instead, we'll handle location-based requests through URL rewriting
        console.info('[DC-PROXY] Using URL rewriting instead of location spoofing');
        
        // Proxify Discord URLs to same-origin so requests go through the local proxy
        function toSameOrigin(url) {
            if (typeof url !== 'string') return url;
            try {
                const u = new URL(url, window.location.origin);
                const host = u.hostname || '';
                // If pointing to discord.com or its subdomains, strip origin so it hits local proxy
                if (host === 'discord.com' || host.endsWith('.discord.com') || host === 'discord.gg') {
                    const path = (u.pathname || '/') + (u.search || '');
                    const finalPath = path.startsWith('/') ? path : '/' + path;
                    console.log(`[DC-PROXY] Rewriting to same-origin: ${url} -> ${finalPath}`);
                    return finalPath;
                }
            } catch (e) {
                // Not a valid absolute URL; leave as-is
            }
            return url;
        }
        
        // Enhanced logging function for debugging
        function logRequest(method, url, isLogin) {
            if (isLogin) {
                console.log(`[DC-PROXY] Login Request: ${method} ${url}`);
            } else if (url.includes('hcaptcha') || url.includes('captcha')) {
                console.log(`[DC-PROXY] Captcha Request: ${method} ${url}`);
            }
        }
        
    // Keep a stable reference to the native fetch for retries
    const originalFetch = window.fetch.bind(window);

    // Enhanced retry mechanism for network requests
    window.fetchWithRetry = function(url, options = {}, maxRetries = 3) {
            return new Promise((resolve, reject) => {
                let retryCount = 0;
                
                function attemptFetch() {
                    originalFetch(url, {
                        ...options,
                        timeout: options.timeout || 10000 // 10 second timeout
                    })
                    .then(response => {
                        if (response.ok || retryCount >= maxRetries) {
                            resolve(response);
                        } else if (response.status >= 500 && retryCount < maxRetries) {
                            // Retry on server errors
                            retryCount++;
                            console.log(`[DC-PROXY] Server error ${response.status}, retrying ${retryCount}/${maxRetries} for ${url}`);
                            setTimeout(attemptFetch, Math.min(1000 * Math.pow(2, retryCount), 5000));
                        } else {
                            resolve(response);
                        }
                    })
                    .catch(error => {
                        if (retryCount < maxRetries && (
                            error.name === 'TypeError' || 
                            error.message.includes('fetch') ||
                            error.message.includes('network') ||
                            error.message.includes('timeout') ||
                            error.message.includes('ECONNRESET') ||
                            error.message.includes('ERR_CONNECTION_TIMED_OUT')
                        )) {
                            retryCount++;
                            console.log(`[DC-PROXY] Network error, retrying ${retryCount}/${maxRetries} for ${url}: ${error.message}`);
                            setTimeout(attemptFetch, Math.min(1000 * Math.pow(2, retryCount), 5000));
                        } else {
                            console.error(`[DC-PROXY] Final retry failure for ${url}:`, error);
                            reject(error);
                        }
                    });
                }
                
                attemptFetch();
            });
        };
        
        // Enhanced response logging
        function logResponse(url, status, response) {
            if (url.includes('/api/v9/auth/login')) {
                console.log(`[DC-PROXY] Login Response: ${status}`, response);
                
                // Show helpful message for captcha errors
                if (status === 400 && response && response.captcha_key) {
                    console.warn(`[DC-PROXY] hCaptcha required for login. This might not work properly with localhost.`);
                    // Display a message to the user about the hCaptcha limitation
                    setTimeout(() => {
                        const captchaMessage = document.createElement('div');
                        captchaMessage.style = 'position:fixed; top:0; left:0; right:0; background:#f04747; color:white; padding:10px; text-align:center; z-index:9999;';
                        captchaMessage.innerHTML = '<b>Proxy Notice:</b> hCaptcha verification required but may not work with localhost. Try logging in directly on Discord first, then use the proxy.';
                        document.body.appendChild(captchaMessage);
                    }, 1000);
                }
            } else if (url.includes('hcaptcha') || url.includes('captcha')) {
                console.log(`[DC-PROXY] Captcha Response: ${status}`);
            }
            
            // For any error responses
            if (status >= 400) {
                console.warn(`[DC-PROXY] Error Response: ${status} for ${url}`, response);
            }
        }
        
        // Override XMLHttpRequest to ensure HTTPS usage
        const OriginalXHR = window.XMLHttpRequest;
        window.XMLHttpRequest = function() {
            const xhr = new OriginalXHR();
            const originalOpen = xhr.open;
            const originalSend = xhr.send;
            let currentUrl = '';
            let isLogin = false;
            
            xhr.open = function(method, url, ...args) {
                currentUrl = url;
                isLogin = url.includes('/api/v9/auth/login');
                const newUrl = toSameOrigin(url);
                logRequest(method, newUrl, isLogin);
                return originalOpen.call(this, method, newUrl, ...args);
            };
            
            xhr.send = function(...args) {
                if (isLogin) {
                    console.log('[DC-PROXY] Sending login request');
                }
                
                // Add response handling
                xhr.addEventListener('load', function() {
                    try {
                        let responseData = null;
                        if (xhr.responseText) {
                            try {
                                responseData = JSON.parse(xhr.responseText);
                            } catch(e) {
                                // Not JSON, that's fine
                            }
                        }
                        logResponse(currentUrl, xhr.status, responseData);
                    } catch(e) {
                        console.error('[DC-PROXY] Error logging response:', e);
                    }
                });
                
                xhr.addEventListener('error', function() {
                    console.error('[DC-PROXY] XHR Error (' + (xhr.status || 'unknown') + ') for ' + currentUrl);
                });
                
                return originalSend.apply(this, args);
            };
            
            return xhr;
        };
        
        // Helper to compute ws tunnel base from current origin (no hardcoded port)
        function getWsTunnelUrl(target) {
            try {
                const origin = window.location.origin; // e.g., http://localhost:12345
                const wsOrigin = origin.startsWith('https') ? origin.replace('https', 'wss') : origin.replace('http', 'ws');
                return `${wsOrigin}/__ws?target=${encodeURIComponent(target)}`;
            } catch (e) {
                // Fallback to relative ws which resolves against current origin
                return `/__ws?target=${encodeURIComponent(target)}`;
            }
        }

        // Create a silent stub WebSocket to suppress noisy local RPC errors (127.0.0.1:646x)
        function createSilentWebSocket(url) {
            console.info(`[DC-PROXY] Suppressing local RPC WebSocket ${url}`);
            const listeners = { open: [], message: [], close: [], error: [] };
            const stub = {
                readyState: 3, // CLOSED
                bufferedAmount: 0,
                extensions: '',
                protocol: '',
                url: String(url || ''),
                binaryType: 'arraybuffer',
                addEventListener(type, cb) { if (listeners[type]) listeners[type].push(cb); },
                removeEventListener(type, cb) { if (listeners[type]) listeners[type] = listeners[type].filter(f => f !== cb); },
                dispatchEvent(ev) { const arr = listeners[ev.type] || []; arr.forEach(fn => { try { fn(ev); } catch {} }); return true; },
                send() { /* no-op */ },
                close(code = 1000, reason = '') { /* no-op, already closed */ },
                set onopen(fn) { this.addEventListener('open', fn); },
                set onmessage(fn) { this.addEventListener('message', fn); },
                set onclose(fn) { this.addEventListener('close', fn); },
                set onerror(fn) { this.addEventListener('error', fn); },
            };
            // Fire a clean close asynchronously to satisfy callers waiting on events
            setTimeout(() => {
                try { stub.dispatchEvent(new Event('close')); } catch {}
            }, 0);
            return stub;
        }

        // Enhanced WebSocket connection with retry logic and routing
        const originalWebSocket = window.WebSocket;
        window.WebSocket = function(url, protocols) {
            console.log(`[DC-PROXY] Creating WebSocket connection to: ${url}`);
            
            // Suppress Discord RPC attempts to local 127.0.0.1:6463-6471 to avoid console spam
            try {
                const u = new URL(url, window.location.href);
                // Discord RPC tries ports 6463-6472
                if ((u.hostname === '127.0.0.1' || u.hostname === 'localhost') && /^64(6[3-9]|7[0-2])$/.test(String(u.port))) {
                    return createSilentWebSocket(url);
                }
            } catch(_e) {
                // not a valid absolute URL; ignore
            }

            // Convert Discord WebSocket URLs to use our proxy tunnel
            let targetUrl = url;
            if (
                typeof url === 'string' && (
                    url.includes('gateway.discord.gg') ||
                    url.includes('remote-auth-gateway.discord.gg') ||
                    url.includes('discord.com')
                )
            ) {
                targetUrl = getWsTunnelUrl(url);
                console.log(`[DC-PROXY] Routing WebSocket through proxy tunnel: ${targetUrl}`);
            }
            
            const ws = new originalWebSocket(targetUrl, protocols);
            
            // Add enhanced error handling and retry logic
            const originalOnError = ws.onerror;
            ws.onerror = function(error) {
                console.error(`[DC-PROXY] WebSocket error for ${url}:`, error);
                
                // Call original error handler if it exists
                if (originalOnError) {
                    originalOnError.call(this, error);
                }
            };
            
            const originalOnClose = ws.onclose;
            ws.onclose = function(event) {
                if (event.code !== 1000) { // 1000 = normal closure
                    console.warn(`[DC-PROXY] WebSocket closed abnormally (${event.code}): ${event.reason}`);
                }
                
                // Call original close handler if it exists
                if (originalOnClose) {
                    originalOnClose.call(this, event);
                }
            };
            
            return ws;
        };
        
    // Override fetch with enhanced error handling and retry logic
        window.fetch = function(resource, init) {
            let requestUrl = '';
            let isLogin = false;
            
            if (typeof resource === 'string') {
                requestUrl = resource;
                resource = toSameOrigin(resource);
            } else if (resource instanceof Request) {
                requestUrl = resource.url;
                const newUrl = toSameOrigin(resource.url);
                const newRequest = new Request(newUrl, resource);
                resource = newRequest;
            }
            
            isLogin = requestUrl.includes('/api/v9/auth/login');
            logRequest(init?.method || 'GET', requestUrl, isLogin);
            
            // Use fetchWithRetry for better error handling
            return window.fetchWithRetry(resource, init, 3)
                .then(response => {
                    // Clone the response so we can both read it and return it
                    const responseClone = response.clone();
                    
                    // Process the response
                    if (isLogin || requestUrl.includes('captcha')) {
                        responseClone.text().then(text => {
                            try {
                                const data = text ? JSON.parse(text) : null;
                                logResponse(requestUrl, response.status, data);
                            } catch(e) {
                                logResponse(requestUrl, response.status, null);
                            }
                        }).catch(err => {
                            console.error('[DC-PROXY] Error logging fetch response:', err);
                        });
                    }
                    
                    return response;
                })
                .catch(error => {
                    console.error('[DC-PROXY] Fetch Error for', requestUrl, error);
                    
                    // For network errors, provide more helpful feedback
                    if (error.name === 'TypeError' || error.message.includes('fetch')) {
                        console.warn('[DC-PROXY] Network connectivity issue detected. Check your internet connection.');
                    }
                    
                    throw error;
                });
        };
        
        console.log("[DC-PROXY] Enhanced client-side handlers with hCaptcha support loaded successfully");
        })();
    </script>
    "#;
    
    // Insert the script into the HTML content
    // Prefer inserting at start of <head> if present so overrides load before Discord scripts
    if let Some(head_pos) = html_content.to_lowercase().find("<head") {
        // Find the end of the opening <head ...>
        let rest = &html_content[head_pos..];
        if let Some(gt) = rest.find('>') {
            let insert_pos = head_pos + gt + 1;
            let (before, after) = html_content.split_at(insert_pos);
            return format!("{}{}{}", before, script, after);
        }
    }
    if let Some(body_end_pos) = html_content.to_lowercase().rfind("</body>") {
        let (before, after) = html_content.split_at(body_end_pos);
        return format!("{}{}{}", before, script, after);
    } else if let Some(head_end_pos) = html_content.to_lowercase().rfind("</head>") {
        let (before, after) = html_content.split_at(head_end_pos);
        return format!("{}{}{}", before, script, after);
    } else {
        // Fallback insertion
        return format!("{}{}", html_content, script);
    }
}

// Helper function to compute the WebSocket accept key
fn compute_websocket_accept(key: &hyper::header::HeaderValue) -> hyper::header::HeaderValue {
    // Convert the key to string
    let key_str = key.to_str().unwrap_or("");
    
    // The magic string from the WebSocket protocol
    let magic_string = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    
    // Concatenate the key and magic string
    let concatenated = format!("{}{}", key_str, magic_string);
    
    // Compute SHA-1 hash
    let mut hasher = Sha1::new();
    hasher.update(concatenated.as_bytes());
    let result = hasher.finalize();
    
    // Convert to base64
    let b64 = general_purpose::STANDARD.encode(result);
    
    // Create header value
    hyper::header::HeaderValue::from_str(&b64).unwrap_or(hyper::header::HeaderValue::from_static(""))
}

fn main() {
    // Setup panic hook to log panics
    setup_panic!();

    // Initialize the logger
    let app_data_dir = get_app_data_dir();
    logger::init_logger(&app_data_dir);

    
    // Check if we're in development mode
    let is_dev_mode = is_development_mode();
    
    // Initialize Sentry for error tracking - disabled in dev mode
    let _sentry_guard = if ENABLE_SENTRY && !is_dev_mode {
        let guard = sentry::init((
            SENTRY_DSN,
            sentry::ClientOptions {
                release: sentry::release_name!(),
                environment: Some(if is_dev_mode { "development" } else { "production" }.into()),
                attach_stacktrace: true,
                before_send: Some(Arc::new(|event| {
                    // Optional hook to process events before sending
                    // For example, removing sensitive information
                    Some(event)
                })),
                before_breadcrumb: Some(Arc::new(|breadcrumb| {
                    // Optional hook to process breadcrumbs
                    Some(breadcrumb)
                })),
                auto_session_tracking: true,
                session_mode: sentry::SessionMode::Request,
                traces_sample_rate: 0.1, // 10% of transactions will be captured
                in_app_exclude: vec![
                    "tauri".into(),
                    "reqwest".into(),
                    "tokio".into(),
                ],
                debug: is_dev_mode,
                shutdown_timeout: Duration::from_secs(5),
                ..Default::default()
            },
        ));
        
        // Log successful initialization
        handle_log_error(
            logger::info("Sentry initialized", "system", Some("Error reporting enabled")),
            "Failed to log Sentry initialization"
        );
        
        Some(guard)
    } else {
        // Log that Sentry is disabled
        let disable_reason = if !ENABLE_SENTRY {
            "globally disabled"
        } else {
            "disabled in development mode"
        };
        
        handle_log_error(
            logger::info(&format!("Sentry {}", disable_reason), "system", Some("Error reporting disabled")),
            "Failed to log Sentry status"
        );
        
        None
    };
    
    // Log application start - wrapped in a result to prevent crashes
    handle_log_error(
        logger::info("Application started", "system", Some("Initializing Tauri application")),
        "Failed to log application start"
    );
    
    // Create the application state
    let app_state = Arc::new(AppState {
        proxies: Mutex::new(Vec::new()),
        active_proxy: Mutex::new(None),
        is_proxy_running: Mutex::new(false),
        proxy_testing_queue: Mutex::new(HashSet::new()),
        fetching_in_progress: Mutex::new(false),
        pipeline_fetching_in_progress: Mutex::new(false),
        pipeline_parsing_in_progress: Mutex::new(false),
        pipeline_testing_in_progress: Mutex::new(false),
        current_proxy_port: Mutex::new(None),
        fetched_bodies: Mutex::new(Vec::new()),
        app_handle: StdMutex::new(None),
        unread_count: Mutex::new(0),
    });
    
    // Load proxies from cache at startup (block on this)
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        if let Err(e) = load_proxies_from_cache(&app_state).await {
            handle_log_error(
                logger::warning(&format!("Failed to load proxies from cache: {}", e), 
                           "ProxyCache", None),
                "Failed to log cache load error"
            );
        }
    });

    tauri::Builder::default()
        .setup(|app| {
            // Create tray menu once at startup
            let show_item = MenuItem::with_id(app, "show", "Show FreeDiscord", true, None::<&str>)?;
            let hide_item = MenuItem::with_id(app, "hide", "Hide Window", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

            let menu = Menu::with_items(app, &[
                &show_item,
                &hide_item,
                &PredefinedMenuItem::separator(app)?,
                &quit_item,
            ])?;

            // Set the tray icon menu and tooltip
            let tray_icon = app.tray_by_id("main").unwrap();
            tray_icon.set_menu(Some(menu))?;
            tray_icon.set_tooltip(Some("FreeDiscord - Discord Proxy Client"))?;

            handle_log_error(
                logger::info("System tray icon created with menu", "system", None),
                "Failed to log tray icon creation"
            );

            // Save app handle into global state for later event emission
            if let Some(state) = app.try_state::<Arc<AppState>>() {
                if let Ok(mut guard) = state.app_handle.lock() {
                    let handle = app.handle().clone();
                    *guard = Some(handle);
                }
            }

            Ok(())
        })
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            logger::log_from_js,
            logger::batch_log_from_js,
            logger::set_logging_enabled,
            logger::get_log_file_path,
            logger::get_recent_logs,
            logger::capture_exception_from_js,
            fetch_proxies,
            test_proxy,
            pipeline_reset,
            pipeline_fetch_sources,
            pipeline_parse_fetched,
            pipeline_test_all,
            start_proxy,
            stop_proxy,
            launch_discord,
            get_active_proxy,
            get_proxy_status,
            get_proxy_base_url,
            get_proxies_paginated,
            get_proxy_testing_status,
            save_proxies_cache,
            load_proxies_cache,
            navigate_to_url,
            return_to_proxy_manager,
            clear_discord_session_data,
            add_custom_proxy,
            delete_proxy,
            clear_proxies_cache,
            test_custom_proxy_detailed,
            open_in_system_browser,
            show_notification,
            set_badge_count,
            flash_window,
            update_tray_notification,
            shutdown_notifications,
            exit_application,
        ])
        .on_window_event(|window, event| {
            match event {
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    // Prevent the default close behavior
                    api.prevent_close();

                    // Hide the window instead of closing it
                    if let Err(e) = window.hide() {
                        handle_log_error(
                            logger::error(&format!("Failed to hide window: {}", e), "system", None),
                            "Failed to log window hide error"
                        );
                    } else {
                        handle_log_error(
                            logger::info("Window minimized to system tray", "system", None),
                            "Failed to log window minimize event"
                        );

                        // Show notification hint about minimizing to tray
                        let app_handle = window.app_handle().clone();
                        tokio::spawn(async move {
                            if let Err(e) = show_tray_minimize_notification(app_handle).await {
                                eprintln!("[DEBUG] Failed to show tray minimize notification: {}", e);
                            }
                        });
                    }
                }
                _ => {}
            }
        })
        .on_menu_event(|app, event| {
            match event.id().as_ref() {
                "show" => {
                    if let Some(window) = app.get_webview_window("main") {
                        if let Err(e) = window.show() {
                            handle_log_error(
                                logger::error(&format!("Failed to show window: {}", e), "system", None),
                                "Failed to log window show error"
                            );
                        } else {
                            let _ = window.set_focus();
                            handle_log_error(
                                logger::info("Window restored from tray menu", "system", None),
                                "Failed to log window restore event"
                            );
                        }
                    }
                }
                "hide" => {
                    if let Some(window) = app.get_webview_window("main") {
                        if let Err(e) = window.hide() {
                            handle_log_error(
                                logger::error(&format!("Failed to hide window: {}", e), "system", None),
                                "Failed to log window hide error"
                            );
                        } else {
                            handle_log_error(
                                logger::info("Window hidden via tray menu", "system", None),
                                "Failed to log window hide event"
                            );
                        }
                    }
                }
                "quit" => {
                    handle_log_error(
                        logger::info("Application quit requested via tray menu", "system", None),
                        "Failed to log application quit event"
                    );
                    std::process::exit(0);
                }
                _ => {}
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");

    // Log application exit - wrapped in a result to prevent crashes
    handle_log_error(
        logger::info("Application exiting", "system", None),
        "Failed to log application exit"
    );
} 