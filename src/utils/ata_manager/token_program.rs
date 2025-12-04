// Token program detection module

use crate::blockchain::rpc_client::RpcClient;
use crate::core::registry::ProgramIds;
use anyhow::{Context, Result};
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;

/// Determines the correct token program ID for a given mint
/// Checks if mint uses Token-2022 (Token Extensions) or standard SPL Token
/// 
/// This function checks the mint account's owner on-chain to determine
/// which token program it uses. This is more reliable than hardcoded lists.
pub async fn get_token_program_for_mint(
    mint: &Pubkey,
    rpc: Option<&Arc<RpcClient>>,
) -> Result<Pubkey> {
    let token_2022_program_id = ProgramIds::token_2022()
        .context("Failed to get Token-2022 program ID from registry")?;
    let standard_token_program_id = spl_token::id();
    
    // If RPC is available, check mint account owner on-chain
    if let Some(rpc_client) = rpc {
        log::debug!("🔍 Fetching mint account {} from RPC to determine token program...", mint);
        match rpc_client.get_account(mint).await {
            Ok(account) => {
                let owner = account.owner;
                log::info!(
                    "📊 Mint account {} fetched: owner={}, lamports={}, data_len={}",
                    mint,
                    owner,
                    account.lamports,
                    account.data.len()
                );
                
                // Primary check - owner program ID (most reliable method)
                if owner == token_2022_program_id {
                    log::info!("✅ Mint {} uses Token-2022 program (owner matches)", mint);
                    return Ok(token_2022_program_id);
                } else if owner == standard_token_program_id {
                    log::info!("✅ Mint {} uses standard SPL Token program (owner matches)", mint);
                    return Ok(standard_token_program_id);
                }
                
                // Validate account data structure
                if account.data.len() < 82 {
                    log::warn!(
                        "⚠️  Mint {} account data too small ({} bytes), expected at least 82 bytes",
                        mint,
                        account.data.len()
                    );
                }
                
                log::warn!(
                    "⚠️  Mint {} has unexpected owner: {} (expected {} or {}), defaulting to standard SPL Token",
                    mint,
                    owner,
                    standard_token_program_id,
                    token_2022_program_id
                );
                return Ok(standard_token_program_id);
            }
            Err(e) => {
                log::warn!(
                    "⚠️  Failed to fetch mint account {} from RPC: {}, trying fallback methods",
                    mint,
                    e
                );
            }
        }
    } else {
        log::debug!("⚠️  No RPC client provided, using fallback method for mint {}", mint);
    }
    
    // Fallback - Check known Token-2022 mints list
    const TOKEN_2022_MINTS: &[&str] = &[
        // Add confirmed Token-2022 mints here as they are discovered
    ];
    
    let mint_str = mint.to_string();
    
    if TOKEN_2022_MINTS.contains(&mint_str.as_str()) {
        log::info!("✅ Mint {} found in known Token-2022 mints list", mint);
        Ok(token_2022_program_id)
    } else {
        log::warn!(
            "⚠️  Mint {} not in known Token-2022 list and RPC check unavailable, defaulting to standard SPL Token. \
             If ATA creation fails with 'Invalid program id', this mint may be Token-2022 and should be added to TOKEN_2022_MINTS.",
            mint
        );
        Ok(standard_token_program_id)
    }
}

