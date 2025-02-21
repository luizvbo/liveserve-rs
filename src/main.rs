use axum::{
    extract::{ws::Message, WebSocketUpgrade},
    routing::get,
    Router,
};
use clap::{Parser, CommandFactory};
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
#[clap(name = "Live Server", about = "A simple development server with live reload capability.")]
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
    if let Err(e) = Cli::command().try_get_matches() {
        eprintln!("Error parsing arguments: {}", e);
        std::process::exit(1);
    }

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
        watcher.watch(root_clone.as_ref(), RecursiveMode::Recursive).unwrap();
    });

    // WebSocket for live reload
    let ws_clients = Arc::new(Mutex::new(vec![]));
    let ws_clients_clone = ws_clients.clone();
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let mut clients = ws_clients_clone.lock().unwrap();
            clients.retain(|client: &tokio::sync::mpsc::UnboundedSender<Message>| {
                client.send(Message::Text("reload".into())).is_ok()
            });
        }
    });

    // Middleware for SPA support
    let root_clone = Arc::clone(&root);
    let spa_handler = move |req: axum::http::Request<axum::body::Body>| {
        let root_clone_inner = Arc::clone(&root_clone);
        async move {
            let path = root_clone_inner.join(req.uri().path().trim_start_matches('/'));
            if path.exists() {
                return axum::response::Response::new(axum::body::Body::from(
                    fs::read(path).await.unwrap(),
                ));
            }
            axum::response::Response::new(axum::body::Body::from(
                fs::read(root_clone_inner.join(spa_entry)).await.unwrap(),
            ))
        }
    };

    // Setup routes
    let app = Router::new()
        .nest_service("/", ServeDir::new(Arc::clone(&root).as_ref()))
        .route("/*path", get(spa_handler))
        .route(
            "/ws",
            get(|ws: WebSocketUpgrade| async move {
                ws.on_upgrade(|socket| async move {
                    let (mut tx, _) = socket.split();
                    tx.send(Message::Text("Live reload enabled".into()))
                        .await
                        .unwrap();
                })
            }),
        );

    let addr = SocketAddr::from(([127, 0, 0, 1], args.port));
    println!("Serving at http://{}", addr);
    println!("Usage:");
    println!("  Run with default settings: live-server");
    println!("  Specify a different root directory: live-server --root ./my-folder");
    println!("  Use a different port: live-server --port 3000");
    println!("  Set a custom SPA entry file: live-server --spa-entry app.html");

    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}
