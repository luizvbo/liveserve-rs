use axum::{
    body::Body,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::fs as tokio_fs;
use tokio::sync::broadcast;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, error, info};

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

/// Handles WebSocket connections, managing client connections and message broadcasting.
pub async fn websocket_handler(
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
pub async fn spa_file_handler(
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

/// Sets up WebSocket broadcast task to relay file change events to WebSocket clients.
pub fn setup_websocket_broadcast(
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

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use hyper::body::to_bytes;
    use std::env;
    use std::sync::Arc;
    use tokio::fs;

    #[tokio::test]
    async fn test_serve_file_injects_ws_script_for_html() {
        // Create a temporary HTML file.
        let mut file_path = env::temp_dir();
        file_path.push("test_file.html");
        let file_content = "<html><body>Hello, world!</body></html>";
        fs::write(&file_path, file_content).await.unwrap();

        let ws_script = "<script>InjectedScript()</script>".to_string();
        let response = crate::server::serve_file(file_path.clone(), ws_script.clone()).await;
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = to_bytes(response.into_body()).await.unwrap();
        let body_str = std::str::from_utf8(&body_bytes).unwrap();
        // Since it's an HTML file, the WebSocket script should be appended.
        assert!(body_str.contains(ws_script.as_str()));

        // Clean up the temporary file.
        let _ = fs::remove_file(file_path).await;
    }

    #[tokio::test]
    async fn test_serve_file_non_html_does_not_inject_ws_script() {
        // Create a temporary non-HTML file.
        let mut file_path = env::temp_dir();
        file_path.push("test_file.txt");
        let file_content = "Plain text content";
        fs::write(&file_path, file_content).await.unwrap();

        let ws_script = "<script>InjectedScript()</script>".to_string();
        let response = crate::server::serve_file(file_path.clone(), ws_script.clone()).await;
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = to_bytes(response.into_body()).await.unwrap();
        let body_str = std::str::from_utf8(&body_bytes).unwrap();
        // For non-HTML files, the content should remain unchanged.
        assert_eq!(body_str, file_content);

        // Clean up the temporary file.
        let _ = fs::remove_file(file_path).await;
    }

    #[tokio::test]
    async fn test_serve_file_returns_error_for_missing_file() {
        // Use a file path that does not exist.
        let mut file_path = env::temp_dir();
        file_path.push("nonexistent_file.html");

        let ws_script = "<script>InjectedScript()</script>".to_string();
        let response = crate::server::serve_file(file_path, ws_script).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_spa_file_handler_serves_existing_file() {
        // Create a temporary HTML file to simulate an SPA file.
        let mut file_path = env::temp_dir();
        file_path.push("spa_test.html");
        let file_content = "<html><body>SPA Content</body></html>";
        fs::write(&file_path, file_content).await.unwrap();

        let ws_script = "<script>InjectedScript()</script>".to_string();
        let root = Arc::new(env::temp_dir());
        let req = Request::builder()
            .uri("/spa_test.html")
            .body(Body::empty())
            .unwrap();

        let response = crate::server::spa_file_handler(req, root.clone(), ws_script.clone()).await;
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = to_bytes(response.into_body()).await.unwrap();
        let body_str = std::str::from_utf8(&body_bytes).unwrap();
        // Check that the WebSocket script was injected into the HTML file.
        assert!(body_str.contains(ws_script.as_str()));

        // Clean up the temporary file.
        let mut cleanup_path = env::temp_dir();
        cleanup_path.push("spa_test.html");
        let _ = fs::remove_file(cleanup_path).await;
    }

    #[tokio::test]
    async fn test_spa_file_handler_returns_not_found_for_missing_file() {
        let root = Arc::new(env::temp_dir());
        let req = Request::builder()
            .uri("/nonexistent.html")
            .body(Body::empty())
            .unwrap();
        let ws_script = "<script>InjectedScript()</script>".to_string();

        let response = crate::server::spa_file_handler(req, root, ws_script).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_setup_websocket_broadcast_keeps_clients() {
        use tokio::time::{sleep, Duration};
        let (tx, _) = tokio::sync::broadcast::channel(16);
        let ws_clients: std::sync::Arc<
            std::sync::Mutex<Vec<tokio::sync::mpsc::UnboundedSender<axum::extract::ws::Message>>>,
        > = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

        // Create a dummy client channel with an explicit type annotation.
        let (client_tx, mut client_rx) =
            tokio::sync::mpsc::unbounded_channel::<axum::extract::ws::Message>();
        ws_clients.lock().unwrap().push(client_tx);

        // Start the broadcast task.
        crate::server::setup_websocket_broadcast(tx.clone(), std::sync::Arc::clone(&ws_clients));

        // Wait briefly to ensure the broadcast task has subscribed.
        sleep(Duration::from_millis(50)).await;

        // Simulate a file change event.
        let event = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any));
        tx.send(event).unwrap();

        // Allow time for the broadcast task to process the event.
        sleep(Duration::from_millis(200)).await;

        // Check that the client received a "rld" message.
        let msg = client_rx.try_recv().unwrap();
        assert_eq!(msg, axum::extract::ws::Message::Text("rld".into()));
    }
}
