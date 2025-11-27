use tokio::sync::broadcast;

pub struct ShutdownManager {
    shutdown_tx: broadcast::Sender<()>,
}

impl ShutdownManager {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1);
        ShutdownManager { shutdown_tx: tx }
    }

    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }

    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.shutdown_tx.subscribe()
    }
}

impl Default for ShutdownManager {
    fn default() -> Self {
        Self::new()
    }
}
