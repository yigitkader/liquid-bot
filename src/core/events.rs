use super::types::{Position, Opportunity};
use solana_sdk::pubkey::Pubkey;
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub enum Event {
    // Discovery
    AccountDiscovered {
        pubkey: Pubkey,
        position: Position,
    },
    AccountUpdated {
        pubkey: Pubkey,
        position: Position,
    },
    
    // Analysis
    OpportunityFound {
        opportunity: Opportunity,
    },
    
    // Validation
    OpportunityApproved {
        opportunity: Opportunity,
    },
    
    // Execution
    TransactionSent {
        signature: String,
    },
    TransactionConfirmed {
        signature: String,
        success: bool,
    },
}

/// Event Bus - Tüm worker'ların haberleştiği merkezi kanal
#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn new(buffer_size: usize) -> Self {
        let (sender, _) = broadcast::channel(buffer_size);
        EventBus { sender }
    }

    pub fn publish(&self, event: Event) -> Result<usize, broadcast::error::SendError<Event>> {
        self.sender.send(event)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.sender.subscribe()
    }
}
