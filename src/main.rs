use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    routing::get,
    Router,
};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use notify::{Event, RecursiveMode, Watcher};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::{fs, sync::watch};
use tower_http::services::ServeDir;

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
    let args = Cli::parse();
    let (tx, mut rx) = watch::channel(());
    let root = Arc::new(PathBuf::from(&args.root));
    let spa_entry = args.spa_entry.clone();

    // File watcher setup
    let watcher_tx = tx.clone();
    let root_clone = Arc::clone(&root);
    tokio::spawn(async move {
        let mut watcher = notify::recommended_watcher(move |res| {
            if let Ok(Event { .. }) = res {
                let _ = watcher_tx.send(());
            }
        })
        .unwrap();
        watcher
            .watch(root_clone.as_ref(), RecursiveMode::Recursive)
            .unwrap();
    });

    // WebSocket for live reload
    let ws_clients: Arc<Mutex<Vec<tokio::sync::mpsc::UnboundedSender<Message>>>> =
        Arc::new(Mutex::new(Vec::new()));
    let ws_clients_clone = Arc::clone(&ws_clients);

    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let mut clients = ws_clients_clone.lock().unwrap();
            clients.retain(|client| client.send(Message::Text("reload".into())).is_ok());
        }
    });

    let ws_clients_clone = Arc::clone(&ws_clients);
    let ws_handler = get(|ws: WebSocketUpgrade| async move {
        ws.on_upgrade(move |socket: WebSocket| async move {
            let (mut tx, mut rx) = socket.split();
            let (client_tx, mut client_rx) = tokio::sync::mpsc::unbounded_channel();

            // Store client connection
            ws_clients_clone.lock().unwrap().push(client_tx);

            // Forward messages
            tokio::spawn(async move {
                while let Some(msg) = client_rx.recv().await {
                    let _ = tx.send(msg).await;
                }
            });

            while rx.next().await.is_some() {}
        })
    });

    // Middleware for SPA support
    let root_clone = Arc::clone(&root);
    let ws_script = r#"
<script>
    const socket = new WebSocket(`ws://${window.location.host}/ws`);
    socket.onmessage = (event) => {
        if (event.data === "reload") {
            window.location.reload();
        }
    };
</script>
"#;

    let spa_handler = move |req: axum::http::Request<axum::body::Body>| {
        let root_clone_inner = Arc::clone(&root_clone);
        let ws_script = ws_script.to_string();

        async move {
            let path = root_clone_inner.join(req.uri().path().trim_start_matches('/'));
            if path.exists() {
                let mut content = fs::read_to_string(&path).await.unwrap();
                if path.extension().map(|ext| ext == "html").unwrap_or(false) {
                    content.push_str(&ws_script);
                }
                return axum::response::Response::new(axum::body::Body::from(content));
            }
            axum::response::Response::new(axum::body::Body::from(
                fs::read(root_clone_inner.join(spa_entry)).await.unwrap(),
            ))
        }
    };

    // Setup routes
    let app = Router::new()
        .nest_service("/", ServeDir::new(Arc::clone(&root).as_ref()))
        .fallback(get(spa_handler))
        .route("/ws", ws_handler);

    let addr = SocketAddr::from(([127, 0, 0, 1], args.port));
    println!("Serving at http://{}", addr);

    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}
