use tokio::sync::broadcast;
use crate::event::Event;

/// Event Bus - Tüm worker'ların haberleştiği merkezi kanal
#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<Event>,
}

impl EventBus {
    /// Yeni bir EventBus oluşturur
    pub fn new(buffer_size: usize) -> Self {
        let (sender, _) = broadcast::channel(buffer_size);
        EventBus { sender }
    }

    /// Event yayınlar
    pub fn publish(&self, event: Event) -> Result<usize, broadcast::error::SendError<Event>> {
        self.sender.send(event)
    }

    /// Yeni bir subscriber oluşturur (her worker kendi receiver'ını alır)
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.sender.subscribe()
    }
}

