// ATA verification module

use crate::blockchain::rpc_client::RpcClient;
use anyhow::Result;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;

/// Check if an ATA exists and verify its owner
pub async fn verify_ata_exists(
    ata: &Pubkey,
    expected_owner: &Pubkey,
    rpc: &Arc<RpcClient>,
) -> Result<bool> {
    match rpc.get_account(ata).await {
        Ok(account) => {
            if account.data.len() >= 64 {
                let owner_bytes = &account.data[32..64];
                if let Ok(owner) = Pubkey::try_from(owner_bytes) {
                    if owner == *expected_owner {
                        return Ok(true);
                    } else {
                        return Err(anyhow::anyhow!(
                            "ATA {} exists but has different owner: {} (expected: {})",
                            ata,
                            owner,
                            expected_owner
                        ));
                    }
                }
            }
            Ok(true) // Account exists but couldn't parse owner
        }
        Err(_) => Ok(false), // Account doesn't exist
    }
}

/// Verify ATA after creation with retries
pub async fn verify_ata_after_creation(
    ata: &Pubkey,
    name: &str,
    rpc: &Arc<RpcClient>,
    max_attempts: u32,
) -> bool {
    for attempt in 1..=max_attempts {
        match rpc.get_account(ata).await {
            Ok(_) => {
                log::info!("✅ {} ATA verified: {}", name, ata);
                return true;
            }
            Err(e) => {
                if attempt < max_attempts {
                    log::debug!(
                        "⚠️  {} ATA verification attempt {} failed, retrying...: {}",
                        name,
                        attempt,
                        e
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                } else {
                    log::warn!(
                        "⚠️  {} ATA verification failed after {} attempts: {}",
                        name,
                        max_attempts,
                        e
                    );
                }
            }
        }
    }
    false
}

