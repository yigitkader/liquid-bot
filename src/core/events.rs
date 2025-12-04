use super::types::{Opportunity, Position};
use solana_sdk::pubkey::Pubkey;
use tokio::sync::broadcast;

/// Event-driven architecture events
/// 
/// This enum represents all events that flow through the event bus.
/// Events are published by producers (Scanner, Analyzer, Validator, Executor)
/// and consumed by subscribers (other components).
/// 
/// The event flow is:
/// 1. Scanner → AccountDiscovered/AccountUpdated
/// 2. Analyzer → OpportunityFound
/// 3. Validator → OpportunityApproved
/// 4. Executor → TransactionSent/TransactionConfirmed
#[derive(Debug, Clone)]
pub enum Event {
    /// Account discovered during initial scan
    AccountDiscovered { 
        pubkey: Pubkey, 
        position: Position 
    },
    
    /// Account updated via WebSocket or polling
    AccountUpdated { 
        pubkey: Pubkey, 
        position: Position 
    },

    /// Liquidation opportunity detected
    OpportunityFound { 
        opportunity: Opportunity 
    },

    /// Opportunity validated and approved for execution
    OpportunityApproved { 
        opportunity: Opportunity 
    },

    /// Transaction sent to blockchain
    TransactionSent { 
        signature: String 
    },
    
    /// Transaction confirmed (success or failure)
    TransactionConfirmed { 
        signature: String, 
        success: bool 
    },
    
    /// WebSocket subscription lost - triggers scanner restart
    /// 
    /// This prevents data loss when WebSocket subscriptions fail during reconnect.
    /// The scanner will restart discovery when this event is received.
    SubscriptionLost { 
        count: usize 
    },
}

/// Event bus for loose coupling between components
/// 
/// Uses tokio::broadcast channel for pub/sub pattern.
/// Multiple subscribers can listen to the same events.
/// 
/// # Buffer Size
/// Default buffer size is 50,000 events to prevent lag during high-volume periods.
/// If buffer is full, oldest events are dropped (broadcast channel behavior).
#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<Event>,
}

impl EventBus {
    /// Create a new event bus with specified buffer size
    /// 
    /// # Parameters
    /// - `buffer_size`: Maximum number of events to buffer (default: 50,000)
    /// 
    /// # Example
    /// ```rust
    /// let event_bus = EventBus::new(50000);
    /// ```
    pub fn new(buffer_size: usize) -> Self {
        let (sender, _) = broadcast::channel(buffer_size);
        EventBus { sender }
    }

    /// Publish an event to all subscribers
    /// 
    /// # Returns
    /// - `Ok(usize)`: Number of active subscribers that received the event
    /// - `Err`: No subscribers (channel is closed)
    /// 
    /// # Example
    /// ```rust
    /// event_bus.publish(Event::AccountDiscovered { pubkey, position })?;
    /// ```
    pub fn publish(&self, event: Event) -> Result<usize, broadcast::error::SendError<Event>> {
        self.sender.send(event)
    }

    /// Subscribe to events
    /// 
    /// Returns a receiver that will receive all events published after subscription.
    /// 
    /// # Example
    /// ```rust
    /// let mut receiver = event_bus.subscribe();
    /// while let Ok(event) = receiver.recv().await {
    ///     // Handle event
    /// }
    /// ```
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.sender.subscribe()
    }
}
