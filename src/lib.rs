pub mod core {
    pub mod config;
    pub mod config_groups;
    pub mod error;
    pub mod events;
    pub mod types;
    pub mod registry;
}

pub mod blockchain {
    pub mod jito;
    pub mod rpc_client;
    pub mod transaction;
    pub mod ws_client;
}

pub mod protocol;

pub mod engine {
    pub mod analyzer;
    pub mod executor;
    pub mod scanner;
    pub mod validator;
}

pub mod strategy {
    pub mod balance_manager;
    pub mod profit_calculator;
    pub mod slippage_estimator;
}

pub mod utils {
    pub mod ata_manager;
    pub mod cache;
    pub mod dependency_validator;
    pub mod error_helpers;
    pub mod error_tracker;
    pub mod helpers;
    pub mod metrics;
}

pub use blockchain::{jito, rpc_client, transaction, ws_client};
pub use core::{config, error, events, types, registry};
pub use engine::{analyzer, executor, scanner, validator};
pub use protocol::Protocol;
pub use strategy::{balance_manager, profit_calculator, slippage_estimator};
pub use utils::{ata_manager, cache, dependency_validator, error_helpers, error_tracker, helpers, metrics};
