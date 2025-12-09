// Wallet balance and value calculation utilities
// Consolidates duplicate wallet balance code from pipeline.rs

use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use spl_associated_token_account::get_associated_token_address;
use std::sync::Arc;

use crate::quotes::get_sol_price_usd_standalone;
use crate::solend::{find_usdc_mint_from_reserves, solend_program_id};

/// Wallet balance information
pub struct WalletBalance {
    pub sol_balance: f64,
    pub sol_value_usd: f64,
    pub usdc_balance: f64,
    pub total_value_usd: f64,
}

/// Get wallet balances (SOL and USDC) with USD values
/// Consolidates duplicate code from log_wallet_balances and get_wallet_value_usd
pub async fn get_wallet_balance(
    rpc: &Arc<RpcClient>,
    wallet_pubkey: &Pubkey,
) -> Result<WalletBalance> {
    // Get SOL balance
    let sol_balance_lamports = rpc
        .get_balance(wallet_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get SOL balance: {}", e))?;
    let sol_balance = sol_balance_lamports as f64 / 1_000_000_000.0;
    
    // Get SOL price from oracle
    let sol_price_usd = match get_sol_price_usd_standalone(rpc).await {
        Some(price) => {
            log::debug!("✅ SOL price from oracle: ${:.2}", price);
            price
        },
        None => {
            log::warn!("⚠️  Failed to get SOL price, using fallback $150");
            150.0
        }
    };
    
    let sol_value_usd = sol_balance * sol_price_usd;
    
    // CRITICAL: Log calculation details for debugging
    log::debug!(
        "SOL balance calculation: {:.6} SOL * ${:.2} = ${:.2}",
        sol_balance,
        sol_price_usd,
        sol_value_usd
    );
    
    // Get USDC balance
    let program_id = solend_program_id()?;
    let usdc_mint = find_usdc_mint_from_reserves(rpc, &program_id)
        .context("Failed to discover USDC mint")?;
    
    let usdc_ata = get_associated_token_address(wallet_pubkey, &usdc_mint);
    
    let usdc_balance_raw = match rpc.get_token_account(&usdc_ata) {
        Ok(Some(account)) => {
            account.token_amount.amount.parse::<u64>().unwrap_or(0)
        }
        Ok(None) => 0,
        Err(_) => 0,
    };
    
    let usdc_balance = usdc_balance_raw as f64 / 1_000_000.0;
    let total_value_usd = sol_value_usd + usdc_balance;
    
    Ok(WalletBalance {
        sol_balance,
        sol_value_usd,
        usdc_balance,
        total_value_usd,
    })
}

/// Log wallet balances to console
pub async fn log_wallet_balances(
    rpc: &Arc<RpcClient>,
    wallet_pubkey: &Pubkey,
) -> Result<()> {
    let balance = get_wallet_balance(rpc, wallet_pubkey).await?;
    
    // Calculate SOL price from balance and value for verification
    let calculated_sol_price = if balance.sol_balance > 0.0 {
        balance.sol_value_usd / balance.sol_balance
    } else {
        0.0
    };
    
    log::info!(
        "💰 Wallet Balances: SOL: {:.6} SOL (${:.2} @ ${:.2}/SOL), USDC: {:.2} USDC, Total: ${:.2}",
        balance.sol_balance,
        balance.sol_value_usd,
        calculated_sol_price,
        balance.usdc_balance,
        balance.total_value_usd
    );
    
    Ok(())
}

/// Get total wallet value in USD (SOL + USDC)
/// Used for risk limit calculations
pub async fn get_wallet_value_usd(
    rpc: &Arc<RpcClient>,
    wallet_pubkey: &Pubkey,
) -> Result<f64> {
    let balance = get_wallet_balance(rpc, wallet_pubkey).await?;
    
    log::debug!(
        "Wallet value: SOL: {:.6} SOL (${:.2}), USDC: {:.2} USDC, Total: ${:.2}",
        balance.sol_balance,
        balance.sol_value_usd,
        balance.usdc_balance,
        balance.total_value_usd
    );
    
    Ok(balance.total_value_usd)
}

