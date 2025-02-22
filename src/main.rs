use axum::extract::ws::Message; // Add this line at the beginning of your imports
use axum::{
    extract::ws::{WebSocket, WebSocketUpgrade},
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
use tower_http::services::ServeDir;
use tracing::{debug, error, info};
use tracing_subscriber::{fmt, EnvFilter}; // Modified import

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
        let ws_clients_clone = Arc::clone(&ws_clients_clone);
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

    // Middleware for SPA support
    let root_clone = Arc::clone(&root);
    let ws_script = r#"
<script>
    console.log("JS Script Injected"); // Simple JS log
    (function() {
        function connect() {
            const socket = new WebSocket(`ws://${window.location.host}/ws`);

            socket.onopen = () => {
                console.log('WebSocket connection opened');
            };

            socket.onmessage = (event) => {
                if (event.data === "reload") {
                    console.log("Reloading page...");
                    window.location.reload();
                }
            };

            socket.onclose = () => {
                console.log("WebSocket disconnected, attempting to reconnect...");
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

    let spa_entry_clone = spa_entry.clone();
    let spa_handler = move |req: axum::http::Request<axum::body::Body>| {
        info!("Handling request for: {:?}", req.uri().path());
        let root_clone_inner = Arc::clone(&root_clone);
        let ws_script = ws_script.to_string();
        let spa_entry = spa_entry_clone.clone();

        async move {
            let path = root_clone_inner.join(req.uri().path().trim_start_matches('/'));
            if path.exists() {
                let mut content = tokio_fs::read_to_string(&path).await.unwrap();
                info!("Checking file extension for: {:?}", path);
                if path.extension().map(|ext| ext == "html").unwrap_or(false) {
                    info!("Injecting WebSocket script into: {:?}", path);
                    content.push_str(&ws_script);
                }
                return axum::response::Response::new(axum::body::Body::from(content));
            }
            axum::response::Response::new(axum::body::Body::from(
                tokio_fs::read(root_clone_inner.join(spa_entry))
                    .await
                    .unwrap(),
            ))
        }
    };

    // Setup routes
    let app = Router::new()
        .route("/ws", ws_handler) // WebSocket first
        .nest_service("/", ServeDir::new(Arc::clone(&root).as_ref())) // Serve files
        .fallback(get(spa_handler)); // Handle unmatched routes

    let addr = SocketAddr::from(([0, 0, 0, 0], args.port));
    info!("Serving at http://{}", addr);
    println!("Server address: http://{}", addr); // Print server address

    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}
