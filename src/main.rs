mod server;
mod watcher;

use axum::{
    body::Body,
    extract::ws::{Message, WebSocketUpgrade},
    routing::get,
    Router,
};
use clap::Parser;
use server::{setup_websocket_broadcast, spa_file_handler, websocket_handler};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;
use tokio::sync::mpsc::UnboundedSender;
use tracing::info;
use tracing_subscriber::{fmt, EnvFilter};
use watcher::setup_file_watcher;

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
