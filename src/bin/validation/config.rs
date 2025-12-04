// Config validation modülü

use anyhow::Result;
use liquid_bot::core::config::Config;
use solana_sdk::pubkey::Pubkey;
use super::{ValidationBuilder, TestResult};

pub async fn validate_config(config: Option<&Config>) -> Result<Vec<TestResult>> {
    let mut builder = ValidationBuilder::new();

    match config {
        Some(config) => {
            builder = builder
                .check("Config Loading", || true)
                .check_with_msg(
                    "RPC HTTP URL",
                    || config.rpc_http_url.starts_with("http://") || config.rpc_http_url.starts_with("https://"),
                    &format!("Valid: {}", config.rpc_http_url),
                    &format!("Invalid format: {}", config.rpc_http_url),
                )
                .check_with_msg(
                    "RPC WS URL",
                    || config.rpc_ws_url.starts_with("ws://") || config.rpc_ws_url.starts_with("wss://"),
                    &format!("Valid: {}", config.rpc_ws_url),
                    &format!("Invalid format: {}", config.rpc_ws_url),
                )
                .check_with_msg(
                    "MIN_PROFIT_USD",
                    || config.min_profit_usd >= 0.0,
                    &format!("Valid: ${}", config.min_profit_usd),
                    &format!("Invalid: ${}", config.min_profit_usd),
                )
                .check_with_msg(
                    "HF_LIQUIDATION_THRESHOLD",
                    || config.hf_liquidation_threshold > 0.0 && config.hf_liquidation_threshold <= 10.0,
                    &format!("Valid: {}", config.hf_liquidation_threshold),
                    &format!("Invalid: {}", config.hf_liquidation_threshold),
                )
                .check_result(
                    "SOLEND_PROGRAM_ID",
                    config.solend_program_id.parse::<Pubkey>(),
                    &format!("Valid: {}", config.solend_program_id),
                )
                .check_with_msg(
                    "Wallet Path",
                    || std::path::Path::new(&config.wallet_path).exists(),
                    &format!("Exists: {}", config.wallet_path),
                    &format!("Not found: {}", config.wallet_path),
                );
        }
        None => {
            builder = builder.check("Config Loading", || false);
        }
    }

    Ok(builder.build())
}

