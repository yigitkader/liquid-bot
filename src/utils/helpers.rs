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
