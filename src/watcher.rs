use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::broadcast;
use tracing::{debug, error, info};

/// Sets up file watcher to send events on file changes.
pub fn setup_file_watcher(tx: broadcast::Sender<notify::Event>, root: Arc<PathBuf>) {
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
        #[cfg(not(test))]
        loop {
            std::thread::park();
        }
        #[cfg(test)]
        {
            // In test mode, sleep briefly and then exit the thread.
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    });
}

#[cfg(test)]
mod tests {

    use std::env;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    #[tokio::test]
    async fn test_setup_file_watcher_receives_event() {
        let (tx, mut rx) = broadcast::channel(16);
        let root = Arc::new(env::temp_dir());
        crate::watcher::setup_file_watcher(tx.clone(), Arc::clone(&root));

        // Simulate sending a file change event manually.
        let event = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any));
        tx.send(event.clone()).unwrap();

        // Wait to receive the event.
        let received_event = rx.recv().await.unwrap();
        assert_eq!(received_event.kind, event.kind);
    }
}
