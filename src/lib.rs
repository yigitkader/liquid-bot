// Core modules
pub mod core {
    pub mod config;
    pub mod events;
    pub mod types;
    pub mod error;
}

// Blockchain modules
pub mod blockchain {
    pub mod rpc_client;
    pub mod ws_client;
    pub mod transaction;
    // rate_limiter removed - not in Structure.md
}

// Protocol modules  
pub mod protocol;

// Engine modules
pub mod engine {
    pub mod scanner;
    pub mod analyzer;
    pub mod validator;
    pub mod executor;
}

// Strategy modules
pub mod strategy {
    pub mod profit_calculator;
    pub mod slippage_estimator;
    pub mod balance_manager;
}

// Utils modules
pub mod utils {
    pub mod cache;
    pub mod metrics;
    pub mod helpers;
}

// Re-exports
pub use core::{config, events, types, error};
pub use blockchain::{rpc_client, ws_client, transaction};
pub use engine::{scanner, analyzer, validator, executor};
pub use strategy::{profit_calculator, slippage_estimator, balance_manager};
pub use utils::{cache, metrics, helpers};
pub use protocol::Protocol;
