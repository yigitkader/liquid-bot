use anyhow::{Context, Result};
use crate::blockchain::rpc_client::RpcClient;
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
