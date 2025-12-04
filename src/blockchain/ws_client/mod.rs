// WebSocket client module - organized into submodules

mod connection;
mod subscription;
mod client;

pub use connection::{ConnectionManager, WsStream};
pub use subscription::{
    SubscriptionManager, SubscriptionInfo, SubscriptionType,
    build_subscription_params, get_subscription_method,
};
pub use client::{WsClient, AccountUpdate};
