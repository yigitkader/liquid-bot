// RPC validation modülü

use anyhow::{Context, Result};
use liquid_bot::blockchain::rpc_client::RpcClient;
use liquid_bot::core::config::Config;
use solana_sdk::pubkey::Pubkey;
// FromStr artık kullanılmıyor
use std::sync::Arc;
use super::{ValidationBuilder, TestResult};

pub async fn validate_rpc_connection(
    rpc_client: &Arc<RpcClient>,
    config: Option<&Config>,
) -> Result<Vec<TestResult>> {
    let mut builder = ValidationBuilder::new();

    // RPC Connection test
    match rpc_client.get_slot().await {
        Ok(slot) => {
            builder = builder.add_result(TestResult::success_with_details(
                "RPC Connection",
                "Connected successfully",
                format!("Current slot: {}", slot),
            ));
        }
        Err(e) => {
            let error_str = e.to_string();
            let error_lower = error_str.to_lowercase();
            
            let error_msg = if error_lower.contains("timeout") {
                format!("RPC timeout - endpoint may be slow or unreachable. Error: {}", e)
            } else if error_lower.contains("connection") || error_lower.contains("econn") {
                format!("RPC connection error - check network and endpoint URL. Error: {}", e)
            } else if error_lower.contains("429") || error_lower.contains("rate limit") {
                format!("RPC rate limit exceeded - consider using premium RPC. Error: {}", e)
            } else {
                format!("RPC connection failed: {}", e)
            };
            
            builder = builder.add_result(TestResult::failure("RPC Connection", &error_msg));
        }
    }

    // RPC Account Fetch test
    let sol_mint_str = config
        .map(|c| c.sol_mint.as_str())
        .unwrap_or("So11111111111111111111111111111111111111112");
    let test_pubkey = sol_mint_str.parse::<Pubkey>()
        .context("Invalid test pubkey")?;
    
    match rpc_client.get_account(&test_pubkey).await {
        Ok(account) => {
            builder = builder.add_result(TestResult::success_with_details(
                "RPC Account Fetch",
                "Successfully fetched account",
                format!("Account data size: {} bytes", account.data.len()),
            ));
        }
        Err(e) => {
            let error_str = e.to_string();
            let error_lower = error_str.to_lowercase();
            
            let error_msg = if error_lower.contains("accountnotfound") || error_lower.contains("account not found") {
                format!("Account not found (this is expected for some accounts). Error: {}", e)
            } else if error_lower.contains("timeout") {
                format!("RPC timeout when fetching account. Error: {}", e)
            } else if error_lower.contains("connection") {
                format!("RPC connection error when fetching account. Error: {}", e)
            } else {
                format!("Failed to fetch account: {}", e)
            };
            
            builder = builder.add_result(TestResult::failure("RPC Account Fetch", &error_msg));
        }
    }

    Ok(builder.build())
}

