use axum::{
    body::Body,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::fs as tokio_fs;
use tokio::sync::broadcast;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, error, info};
use tracing_subscriber::{fmt, EnvFilter};

/// Command-line arguments parser
#[derive(Parser, Debug)]
#[clap(
    name = "Live Serve-rs",
    about = "A simple development server with live reload capability."
)]
struct Cli {
    /// The port to run the server on
    #[clap(short, long, default_value = "8080")]
    port: u16,
    /// The root directory to serve files from
    #[clap(short, long, default_value = "./public")]
    root: String,
    /// The entry file for SPA fallback
    #[clap(long, default_value = "index.html")]
    spa_entry: String,
}

/// Handles WebSocket connections, managing client connections and message broadcasting.
async fn websocket_handler(
    ws: WebSocketUpgrade,
    clients: Arc<Mutex<Vec<UnboundedSender<Message>>>>,
) -> Response {
    ws.on_upgrade(move |socket: WebSocket| handle_socket(socket, clients))
}

/// Handles each WebSocket connection, adding new clients and managing message reception.
async fn handle_socket(socket: WebSocket, clients: Arc<Mutex<Vec<UnboundedSender<Message>>>>) {
    let (mut sender, mut receiver) = socket.split();
    let (client_sender, mut client_receiver) = tokio::sync::mpsc::unbounded_channel();

    // Add new client to the list of connected clients
    clients.lock().unwrap().push(client_sender);
    debug!(
        "WebSocket client connected. Total clients: {}",
        clients.lock().unwrap().len()
    );
    println!("WebSocket client connected (from handler)");

    // Spawn a task to forward messages from client receiver to WebSocket sender
    let mut send_task = tokio::spawn(async move {
        while let Some(msg) = client_receiver.recv().await {
            if sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Spawn a task to keep connection alive and handle incoming messages (though we expect none in this setup)
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(_)) = receiver.next().await {
            // Just consume messages to keep connection alive, no specific action needed for messages from client in this server
        }
    });

    // Wait for either task to complete, which indicates connection close
    tokio::select! {
        _ = (&mut send_task) => {
            debug!("Send task completed for a WebSocket client.");
        },
        _ = (&mut recv_task) => {
            debug!("Receive task completed for a WebSocket client.");
        },
    };

    debug!("Closing WebSocket connection for a client.");
    println!("WebSocket client connection closed (from handler)");
}

/// Serves static files from the specified root directory, injecting live-reload script into HTML files.
async fn spa_file_handler(
    req: axum::http::Request<Body>,
    root: Arc<PathBuf>,
    ws_script: String,
) -> Response {
    info!("Handling request for: {:?}", req.uri().path());
    let path = root.join(req.uri().path().trim_start_matches('/'));
    debug!("Attempting to serve path: {:?}", path);

    if path.is_file() {
        debug!("Path is a file: {:?}", path);
        serve_file(path, ws_script).await.into_response()
    } else {
        debug!("Path is NOT a file or doesn't exist: {:?}", path);
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("File not found"))
            .unwrap()
            .into_response()
    }
}

/// Serves a single file, injecting the WebSocket script if it's an HTML file.
async fn serve_file(path: PathBuf, ws_script: String) -> Response {
    match tokio_fs::read_to_string(&path).await {
        Ok(mut content) => {
            if path.extension().map(|ext| ext == "html").unwrap_or(false) {
                info!("Injecting WebSocket script into HTML file: {:?}", path);
                content.push_str(&ws_script);
                debug!("HTML content with script injected, serving.");
                Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "text/html; charset=utf-8")
                    .body(Body::from(content))
                    .unwrap()
                    .into_response()
            } else {
                debug!("Serving static file (non-HTML): {:?}", path);
                serve_static_file(path).await.into_response()
            }
        }
        Err(e) => {
            error!("Error reading file: {:?} - {:?}", path, e);
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from(format!("Error reading file: {:?}", e)))
                .unwrap()
                .into_response()
        }
    }
}

/// Serves static files with proper mime type detection.
async fn serve_static_file(path: PathBuf) -> Response {
    match tokio_fs::read(&path).await {
        Ok(bytes) => {
            let mime_type = mime_guess::from_path(path.clone()).first_or_octet_stream();
            Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", mime_type.to_string())
                .body(Body::from(bytes))
                .unwrap()
                .into_response()
        }
        Err(e) => {
            error!("Error reading static file: {:?} - {:?}", path, e);
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from(format!("Error reading file: {:?}", e)))
                .unwrap()
                .into_response()
        }
    }
}

/// Sets up file watcher to send events on file changes.
fn setup_file_watcher(tx: broadcast::Sender<notify::Event>, root: Arc<PathBuf>) {
    let tx_watcher = tx.clone();
    let root_clone = Arc::clone(&root);
    tokio::task::spawn_blocking(move || {
        println!("File watcher thread started");
        let mut watcher = RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| match res {
                Ok(event) => {
                    debug!("File change detected: {:?}", event);
                    println!("File change event detected by watcher thread");
                    if tx_watcher.send(event).is_err() {
                        error!("Failed to send file change event through broadcast channel");
                    }
                }
                Err(e) => {
                    error!("Watch error: {:?}", e);
                }
            },
            Config::default(),
        )
        .expect("Failed to create file watcher");

        if let Err(e) = watcher.watch(root_clone.as_ref(), RecursiveMode::Recursive) {
            error!("Failed to watch directory: {:?}", e);
        } else {
            info!("Watching directory: {:?}", root_clone);
        }
        // Keep the watcher thread alive
        loop {
            std::thread::park();
        }
    });
}

/// Sets up WebSocket broadcast task to relay file change events to WebSocket clients.
fn setup_websocket_broadcast(
    tx: broadcast::Sender<notify::Event>,
    ws_clients: Arc<Mutex<Vec<UnboundedSender<Message>>>>,
) {
    let ws_clients_clone = Arc::clone(&ws_clients);
    tokio::spawn(async move {
        println!("WebSocket broadcast task started");
        let mut rx_ws = tx.subscribe();
        loop {
            match rx_ws.recv().await {
                Ok(_event) => {
                    // Debounce file change events
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    // Drain any pending events to avoid multiple reloads for rapid changes
                    while rx_ws.try_recv().is_ok() {}

                    debug!("Broadcasting reload message after debounce");
                    let mut clients = ws_clients_clone.lock().unwrap();
                    debug!("Number of connected WebSocket clients: {}", clients.len());
                    clients.retain(|client| {
                        let send_result = client.send(Message::Text("rld".into()));
                        if send_result.is_err() {
                            debug!("Failed to send reload message to a disconnected client");
                            false // Remove disconnected clients
                        } else {
                            debug!("Successfully sent reload message to a client");
                            true // Keep connected clients
                        }
                    });
                }
                Err(err) => {
                    error!("Error receiving broadcast message: {:?}", err);
                    break; // Exit loop on error
                }
            }
        }
    });
}

#[tokio::main]
async fn main() {
    println!("Server starting...");
    // Initialize tracing with environment filter for logging
    fmt().with_env_filter(EnvFilter::from_default_env()).init();
    info!("Tracing initialized with env filter");

    // Parse command-line arguments
    let args = Cli::parse();

    // Create a broadcast channel for file watcher events (buffer size 16)
    let (tx, _) = broadcast::channel(16);
    // Create shared path buffer for root directory
    let root = Arc::new(PathBuf::from(&args.root));

    // Setup file watcher in a separate blocking task
    setup_file_watcher(tx.clone(), Arc::clone(&root));

    // Initialize WebSocket clients list
    let ws_clients: Arc<Mutex<Vec<UnboundedSender<Message>>>> = Arc::new(Mutex::new(Vec::new()));
    // Setup WebSocket broadcast task
    setup_websocket_broadcast(tx.clone(), Arc::clone(&ws_clients));

    // Define WebSocket injection script
    let ws_script = r#"
<script>
    console.log("JS Script Injected");
    (function() {
        function connect() {
            console.log("Before connect() call");
            const socket = new WebSocket(`ws://${window.location.host}/ws`);
            console.log("Connecting to WebSocket...");
            socket.onopen = () => {
                console.log('WebSocket connection opened');
            };
            socket.onmessage = (event) => {
                console.log("WebSocket message received (raw data):", event.data);
                console.log("Type of event.data:", typeof event.data);
                const messageData = String(event.data);
                console.log("WebSocket message as String:", messageData);
                if (messageData === "rld") {
                    console.log("Entering reload block");
                    console.log("Reloading page NOW... (HARD RELOAD)");
                    console.log("Page reload function called (HARD RELOAD)");
                    // Only reload if not already in the process of reloading
                    window.location.reload(true);
                } else {
                    console.log("Received message is NOT 'rld':", messageData);
                }
            };
            socket.onclose = () => {
                console.log("WebSocket disconnected, attempting to reconnect...");
                setTimeout(connect, 1000);
            };
            socket.onerror = (error) => {
                console.error('WebSocket error:', error);
                console.error('WebSocket onerror event:', event);
                console.error('WebSocket onerror type:', typeof error);
            };
        }
        connect();
    })();
</script>
"#;

    // Define routes
    let app = Router::new()
        .route(
            "/ws",
            get({
                let clients = Arc::clone(&ws_clients);
                move |ws: WebSocketUpgrade| websocket_handler(ws, clients)
            }),
        )
        .route(
            "/*path",
            get({
                let root_clone = Arc::clone(&root);
                let ws_script_clone = ws_script.to_string();
                move |req: axum::http::Request<Body>| {
                    spa_file_handler(req, root_clone, ws_script_clone)
                }
            }),
        );

    // Start the axum server
    let addr = SocketAddr::from(([0, 0, 0, 0], args.port));
    info!("Serving at http://{}/{}", addr, args.spa_entry);
    println!("Serving at http://{}/{}", addr, args.spa_entry);

    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}
