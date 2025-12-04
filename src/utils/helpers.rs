use anyhow::{Context, Result};
use crate::blockchain::rpc_client::RpcClient;
use crate::core::config::Config;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;

pub fn parse_pubkey(s: &str) -> Result<solana_sdk::pubkey::Pubkey> {
    s.parse()
        .map_err(|e| anyhow::anyhow!("Invalid pubkey: {}", e))
}

pub fn parse_pubkey_opt(s: &str) -> Option<solana_sdk::pubkey::Pubkey> {
    s.parse().ok()
}

/// Read mint decimals from mint account
/// 
/// SPL Token mint account layout:
/// - bytes 0-36: mint_authority (Option<Pubkey>)
/// - bytes 36-44: supply (u64)
/// - bytes 44-45: decimals (u8)
/// - bytes 45-46: is_initialized (bool)
/// - bytes 46-78: freeze_authority (Option<Pubkey>)
/// 
/// Returns:
/// - Ok(u8): Token decimals (0-255)
/// - Err: Network/RPC errors or invalid account data
pub async fn read_mint_decimals(
    mint: &Pubkey,
    rpc: &Arc<RpcClient>,
) -> Result<u8> {
    let account = rpc.get_account(mint).await
        .context("Failed to fetch mint account")?;
    
    if account.data.len() < 45 {
        return Err(anyhow::anyhow!(
            "Invalid mint account data: expected at least 45 bytes, got {} bytes for mint {}",
            account.data.len(),
            mint
        ));
    }
    
    // Decimals is at offset 44 (after mint_authority: 36 bytes + supply: 8 bytes)
    let decimals = account.data[44];
    
    log::debug!(
        "read_mint_decimals: mint {} has {} decimals",
        mint,
        decimals
    );
    
    Ok(decimals)
}

/// Convert USD amount (in micro-USD, i.e., USD × 1e6) to token amount
/// 
/// Formula: token_amount = (usd_amount_micro / 1e6 / price) * 10^decimals
/// 
/// Parameters:
/// - usd_amount_micro: USD amount in micro-USD (USD × 1e6)
/// - price: Token price in USD (from oracle)
/// - decimals: Token decimals (from mint account)
/// 
/// Returns:
/// - Ok(u64): Token amount in smallest unit (lamports/token units)
/// - Err: Invalid parameters (price <= 0, etc.)
pub fn usd_to_token_amount(
    usd_amount_micro: u64,
    price: f64,
    decimals: u8,
) -> Result<u64> {
    if price <= 0.0 {
        return Err(anyhow::anyhow!(
            "Invalid price: {} (must be > 0)",
            price
        ));
    }
    
    // Convert micro-USD to USD
    let usd_amount = usd_amount_micro as f64 / 1_000_000.0;
    
    // Calculate token amount: USD / price
    let token_amount = usd_amount / price;
    
    // Convert to smallest unit: multiply by 10^decimals
    let token_amount_smallest_unit = token_amount * 10_f64.powi(decimals as i32);
    
    // Round to u64 (truncate fractional part)
    let token_amount_u64 = token_amount_smallest_unit as u64;
    
    log::debug!(
        "usd_to_token_amount: usd_amount_micro={}, price={:.6}, decimals={}, token_amount={:.6}, token_amount_u64={}",
        usd_amount_micro,
        price,
        decimals,
        token_amount,
        token_amount_u64
    );
    
    Ok(token_amount_u64)
}

/// Convert USD amount (in micro-USD) to token amount for a given mint
/// 
/// This is a convenience function that:
/// 1. Gets oracle price for the mint
/// 2. Reads mint decimals
/// 3. Converts USD×1e6 to token amount
/// 
/// This function eliminates code duplication between validator and executor.
/// 
/// Parameters:
/// - mint: Token mint address
/// - usd_amount_micro: USD amount in micro-USD (USD × 1e6)
/// - rpc: RPC client for on-chain queries
/// - config: Configuration (for oracle account resolution)
/// 
/// Returns:
/// - Ok(u64): Token amount in smallest unit
/// - Err: Network/RPC errors, oracle errors, or conversion errors
pub async fn convert_usd_to_token_amount_for_mint(
    mint: &Pubkey,
    usd_amount_micro: u64,
    rpc: &Arc<RpcClient>,
    config: &Config,
) -> Result<u64> {
    use crate::protocol::oracle::{get_oracle_accounts_from_mint, read_oracle_price};
    
    // Get oracle price for mint
    let (pyth_oracle, switchboard_oracle) = get_oracle_accounts_from_mint(mint, Some(config))
        .context("Failed to get oracle accounts for mint")?;
    
    let price_data = read_oracle_price(
        pyth_oracle.as_ref(),
        switchboard_oracle.as_ref(),
        Arc::clone(rpc),
        Some(config),
    )
    .await
    .context("Failed to read oracle price for mint")?
    .ok_or_else(|| anyhow::anyhow!("No oracle price data available for mint {}", mint))?;
    
    // Get decimals for mint
    let decimals = read_mint_decimals(mint, rpc)
        .await
        .context("Failed to read decimals for mint")?;
    
    // Convert USD×1e6 to token amount
    usd_to_token_amount(usd_amount_micro, price_data.price, decimals)
        .context("Failed to convert USD amount to token amount")
}

/// Read ATA (Associated Token Account) balance from on-chain account
/// 
/// This helper function encapsulates the common pattern of:
/// 1. Fetching account from RPC
/// 2. Handling AccountNotFound errors (returns 0 balance)
/// 3. Validating account data structure (>= 72 bytes for SPL token account)
/// 4. Reading balance from bytes 64..72 (standard SPL token account layout)
/// 
/// Returns:
/// - Ok(u64): Token balance in lamports (0 if account doesn't exist)
/// - Err: Network/RPC errors (not AccountNotFound)
/// 
/// This is used throughout the codebase to avoid code duplication and ensure
/// consistent error handling for ATA balance reads.
pub async fn read_ata_balance(
    ata: &Pubkey,
    rpc: &Arc<RpcClient>,
) -> Result<u64> {
    match rpc.get_account(ata).await {
        Ok(acc) => {
            if acc.data.len() < 72 {
                return Err(anyhow::anyhow!(
                    "Invalid token account data: expected at least 72 bytes, got {} bytes for ATA {}",
                    acc.data.len(),
                    ata
                ));
            }
            
            let balance_bytes: [u8; 8] = acc.data[64..72]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to read balance bytes from ATA {}", ata))?;
            
            let balance = u64::from_le_bytes(balance_bytes);
            Ok(balance)
        }
        Err(e) => {
            let error_msg = e.to_string();
            if error_msg.contains("AccountNotFound") || error_msg.contains("account not found") {
                // ATA doesn't exist - return 0 balance (this is expected for new ATAs)
                log::debug!(
                    "read_ata_balance: ATA {} not found, returning 0 balance",
                    ata
                );
                Ok(0)
            } else {
                // Other errors (network issues, RPC errors, etc.) should be propagated
                Err(e).with_context(|| format!("Failed to read ATA balance for {}", ata))
            }
        }
    }
}
