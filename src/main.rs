use axum::{
    body::Body, // Import Body
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::StatusCode,   // Import StatusCode
    response::Response, // Import Response
    routing::get,
    Router,
};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::mpsc,
    sync::{Arc, Mutex},
    thread,
};
use tokio::fs as tokio_fs;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, error, info};
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Parser)]
#[clap(
    name = "Live Server",
    about = "A simple development server with live reload capability."
)]
struct Cli {
    /// The port to run the server on (default: 8080)
    #[clap(short, long, default_value = "8080")]
    port: u16,

    /// The root directory to serve files from (default: ./public)
    #[clap(short, long, default_value = "./public")]
    root: String,

    /// The entry file for SPA fallback (default: index.html)
    #[clap(long, default_value = "index.html")]
    spa_entry: String,
}

#[tokio::main]
async fn main() {
    println!("Server starting..."); // Added basic startup print

    fmt()
        .with_env_filter(EnvFilter::from_default_env()) // Use environment variable for filtering
        .init();

    info!("Tracing initialized with env filter"); // Info log after init

    let args = Cli::parse();
    let (tx, rx) = mpsc::channel();
    let rx = Arc::new(Mutex::new(rx));
    let root = Arc::new(PathBuf::from(&args.root));
    let spa_entry = args.spa_entry.clone();

    // File watcher setup
    let watcher_tx = tx.clone();
    let root_clone = Arc::clone(&root);
    let rx_clone = Arc::clone(&rx);

    // Run the file watcher in a blocking thread
    thread::spawn(move || {
        println!("File watcher thread started"); // Basic print in watcher thread
        let mut watcher = RecommendedWatcher::new(watcher_tx, Config::default())
            .expect("Failed to create file watcher");
        if let Err(e) = watcher.watch(root_clone.as_ref(), RecursiveMode::Recursive) {
            error!("Failed to watch directory: {:?}", e);
        } else {
            info!("Watching directory: {:?}", root_clone);
        }

        for res in rx_clone.lock().unwrap().iter() {
            match res {
                Ok(event) => {
                    debug!("File change detected: {:?}", event);
                    println!("File change event detected by watcher thread"); // Basic print for file change
                }
                Err(e) => error!("Watch error: {:?}", e),
            }
        }
    });

    // WebSocket for live reload
    let ws_clients: Arc<Mutex<Vec<UnboundedSender<Message>>>> = Arc::new(Mutex::new(Vec::new()));
    let ws_clients_clone: Arc<Mutex<Vec<UnboundedSender<Message>>>> = Arc::clone(&ws_clients);
    let rx_clone = Arc::clone(&rx);

    tokio::spawn(async move {
        println!("WebSocket broadcast task started"); // Basic print for websocket task
        for event in rx_clone.lock().unwrap().iter() {
            debug!(
                "Broadcasting reload message triggered by event: {:?}",
                event
            );
            let mut clients = ws_clients_clone.lock().unwrap();
            debug!("Number of connected WebSocket clients: {}", clients.len());
            clients.retain(|client| {
                let send_result = client.send(Message::Text("reload".into()));
                if send_result.is_err() {
                    debug!("Failed to send reload message to a client (likely disconnected)");
                    false
                } else {
                    debug!("Successfully sent reload message to a client");
                    true
                }
            });
        }
    });

    let ws_clients_clone: Arc<Mutex<Vec<UnboundedSender<Message>>>> = Arc::clone(&ws_clients);
    let ws_handler = get(|ws: WebSocketUpgrade| async move {
        let ws_clients_clone: Arc<Mutex<Vec<UnboundedSender<Message>>>> =
            Arc::clone(&ws_clients_clone);
        ws.on_upgrade(move |socket: WebSocket| async move {
            let (mut tx, mut rx) = socket.split();
            let (client_tx, mut client_rx) = tokio::sync::mpsc::unbounded_channel();

            // Store client connection
            ws_clients_clone.lock().unwrap().push(client_tx);
            debug!(
                "WebSocket client connected. Total clients: {}",
                ws_clients_clone.lock().unwrap().len()
            );
            println!("WebSocket client connected (from handler)"); // Basic print in handler

            // Forward messages (though we are not expecting messages from client in this example)
            tokio::spawn(async move {
                while let Some(msg) = client_rx.recv().await {
                    debug!("Received message from WebSocket client: {:?}", msg);
                    if tx.send(msg).await.is_err() {
                        break; // Client disconnected
                    }
                }
            });

            while rx.next().await.is_some() {}
            debug!("WebSocket connection closed for a client.");
            println!("WebSocket client connection closed (from handler)"); // Basic print in handler
        })
    });

    let root_clone = Arc::clone(&root);
    let ws_script = r#"
<script>
    console.log("JS Script Injected - Version 3"); // Version marker

    (function() {
        function connect() {
            console.log("Before connect() call - Version 3"); // Log before connect

            const socket = new WebSocket(`ws://${window.location.host}/ws`);
            console.log("Connecting to WebSocket... - Version 3"); // Log during connect

            socket.onopen = () => {
                console.log('WebSocket connection opened - Version 3');
            };

            socket.onmessage = (event) => {
                console.log("WebSocket message received (raw data):", event.data); // Raw data log
                console.log("Type of event.data:", typeof event.data); // Type log

                const messageData = String(event.data); // Force string conversion
                console.log("WebSocket message as String:", messageData); // Log as String

                if (messageData === "reload") { // Use the string version for comparison
                    console.log("Entering reload block - Version 3");
                    console.log("Reloading page NOW... (HARD RELOAD) - Version 3");
                    console.log("Page reload function called (HARD RELOAD) - Version 3");
                    window.location.reload();
                } else {
                    console.log("Received message is NOT 'reload':", messageData); // Log if not "reload"
                }
            };

            socket.onclose = () => {
                console.log("WebSocket disconnected, attempting to reconnect... - Version 3");
                setTimeout(connect, 1000);
            };

            socket.onerror = (error) => {
                console.error('WebSocket error:', error);
            };
        }
        connect();
    })();
</script>
"#;

    let spa_handler = move |req: axum::http::Request<axum::body::Body>| {
        info!("Handling request for: {:?}", req.uri().path());
        let root_clone_inner = Arc::clone(&root_clone);
        let ws_script = ws_script.to_string();
        let spa_entry_clone = spa_entry.clone();

        async move {
            let path = root_clone_inner.join(req.uri().path().trim_start_matches('/'));
            debug!("Attempting to serve path: {:?}", path);

            if path.is_file() {
                debug!("Path is a file: {:?}", path);
                // Clone path *before* reading!
                let path_clone_for_read = path.clone();
                match tokio_fs::read_to_string(&path_clone_for_read).await {
                    Ok(mut content) => {
                        if path.extension().map(|ext| ext == "html").unwrap_or(false) {
                            info!("Injecting WebSocket script into HTML file: {:?}", path);
                            content.push_str(&ws_script);
                            content.push_str(""); // Marker comment
                            debug!("HTML content with script injected, serving.");
                            Response::builder()
                                .status(StatusCode::OK)
                                .header("Content-Type", "text/html; charset=utf-8")
                                .body(Body::from(content))
                                .unwrap()
                        } else {
                            debug!("Serving static file (non-HTML): {:?}", path);
                            match tokio_fs::read(&path).await {
                                Ok(bytes) => {
                                    let mime_type =
                                        mime_guess::from_path(path.clone()).first_or_octet_stream();
                                    Response::builder()
                                        .status(StatusCode::OK)
                                        .header("Content-Type", mime_type.to_string())
                                        .body(Body::from(bytes))
                                        .unwrap()
                                }
                                Err(e) => {
                                    error!("Error reading static file: {:?} - {:?}", path, e);
                                    Response::builder()
                                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                                        .body(Body::from(format!("Error reading file: {:?}", e)))
                                        .unwrap()
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("Error reading file: {:?} - {:?}", path, e);
                        Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(Body::from(format!("Error reading file: {:?}", e)))
                            .unwrap()
                    }
                }
            } else {
                debug!("Path is NOT a file or doesn't exist: {:?}", path);
                Response::builder()
                    .status(StatusCode::NOT_FOUND) // Explicit 404 for not found
                    .body(Body::from("File not found"))
                    .unwrap()
            }
        }
    };

    // Modified Routes: spa_handler for all GET requests, WebSocket route
    let app = Router::new()
        .route("/ws", ws_handler) // WebSocket route remains
        .route("/*path", get(spa_handler)); // spa_handler for all GET requests

    let addr = SocketAddr::from(([0, 0, 0, 0], args.port));
    info!("Serving at http://{}", addr);
    println!("Serving at http://{}", addr);

    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}
