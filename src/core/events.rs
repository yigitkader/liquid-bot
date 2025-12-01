use super::types::{Position, Opportunity};
use solana_sdk::pubkey::Pubkey;
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub enum Event {
    AccountDiscovered {
        pubkey: Pubkey,
        position: Position,
    },
    AccountUpdated {
        pubkey: Pubkey,
        position: Position,
    },
    
    OpportunityFound {
        opportunity: Opportunity,
    },
    
    OpportunityApproved {
        opportunity: Opportunity,
    },
    
    TransactionSent {
        signature: String,
    },
    TransactionConfirmed {
        signature: String,
        success: bool,
    },
}

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
