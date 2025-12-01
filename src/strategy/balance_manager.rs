use crate::blockchain::rpc_client::RpcClient;
use crate::core::config::Config;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use anyhow::Result;

pub struct BalanceManager {
    reserved: Arc<RwLock<HashMap<Pubkey, u64>>>,
    rpc: Arc<RpcClient>,
    wallet: Pubkey,
    config: Option<Config>,
}

impl BalanceManager {
    pub fn new(rpc: Arc<RpcClient>, wallet: Pubkey) -> Self {
        BalanceManager {
            reserved: Arc::new(RwLock::new(HashMap::new())),
            rpc,
            wallet,
            config: None,
        }
    }

    pub fn with_config(mut self, config: Config) -> Self {
        self.config = Some(config);
        self
    }

    pub async fn get_available_balance(&self, mint: &Pubkey) -> Result<u64> {
        use crate::protocol::solend::accounts::get_associated_token_address;
        let ata = get_associated_token_address(&self.wallet, mint, self.config.as_ref())?;
        
        // Handle AccountNotFound gracefully - if ATA doesn't exist, balance is 0
        // This is expected behavior: ATA doesn't exist = no token account = balance is 0
        let account = match self.rpc.get_account(&ata).await {
            Ok(acc) => acc,
            Err(e) => {
                let error_msg = e.to_string();
                if error_msg.contains("AccountNotFound") || error_msg.contains("account not found") {
                    // ATA doesn't exist yet - this is normal, balance is 0
                    // Note: ATA can be created automatically during transaction if needed
                    log::debug!(
                        "BalanceManager: ATA not found for mint {} (ata={}), wallet={}. Returning 0 balance (ATA will be auto-created during transaction if needed)",
                        mint,
                        ata,
                        self.wallet
                    );
                    let reserved = self.reserved.read().await.get(mint).copied().unwrap_or(0);
                    // Available = 0 - reserved (which will be 0 if reserved is 0)
                    return Ok(0u64.saturating_sub(reserved));
                }
                // Other errors (network issues, RPC errors, etc.) should be propagated
                return Err(e);
            }
        };
        
        if account.data.len() < 72 {
            return Err(anyhow::anyhow!("Invalid token account data"));
        }
        
        let balance_bytes: [u8; 8] = account.data[64..72].try_into()
            .map_err(|_| anyhow::anyhow!("Failed to read balance from token account"))?;
        let actual = u64::from_le_bytes(balance_bytes);
        
        let reserved = self.reserved.read().await.get(mint).copied().unwrap_or(0);
        let available = actual.saturating_sub(reserved);
        Ok(available)
    }

    /// Get available balance while holding a read lock on reserved map.
    /// This is used internally to avoid double-locking.
    async fn get_available_balance_locked(
        &self,
        mint: &Pubkey,
        reserved: &HashMap<Pubkey, u64>,
    ) -> Result<u64> {
        use crate::protocol::solend::accounts::get_associated_token_address;
        let ata = get_associated_token_address(&self.wallet, mint, self.config.as_ref())?;
        
        // Handle AccountNotFound gracefully - if ATA doesn't exist, balance is 0
        // This is expected behavior: ATA doesn't exist = no token account = balance is 0
        let account = match self.rpc.get_account(&ata).await {
            Ok(acc) => acc,
            Err(e) => {
                let error_msg = e.to_string();
                if error_msg.contains("AccountNotFound") || error_msg.contains("account not found") {
                    // ATA doesn't exist yet - this is normal, balance is 0
                    // Note: ATA can be created automatically during transaction if needed
                    log::debug!(
                        "BalanceManager: ATA not found for mint {} (ata={}), wallet={}. Returning 0 balance (ATA will be auto-created during transaction if needed)",
                        mint,
                        ata,
                        self.wallet
                    );
                    let reserved_amount = reserved.get(mint).copied().unwrap_or(0);
                    // Available = 0 - reserved (which will be 0 if reserved is 0)
                    return Ok(0u64.saturating_sub(reserved_amount));
                }
                // Other errors (network issues, RPC errors, etc.) should be propagated
                return Err(e);
            }
        };
        
        if account.data.len() < 72 {
            return Err(anyhow::anyhow!("Invalid token account data"));
        }
        
        let balance_bytes: [u8; 8] = account.data[64..72].try_into()
            .map_err(|_| anyhow::anyhow!("Failed to read balance from token account"))?;
        let actual = u64::from_le_bytes(balance_bytes);
        
        let reserved_amount = reserved.get(mint).copied().unwrap_or(0);
        let available = actual.saturating_sub(reserved_amount);
        Ok(available)
    }

    /// Atomically reserve balance, preventing race conditions.
    /// This method holds a write lock during both balance check and reservation,
    /// ensuring no other thread can reserve the same balance concurrently.
    pub async fn reserve(&self, mint: &Pubkey, amount: u64) -> Result<ReservationGuard> {
        // Acquire write lock FIRST - this prevents race conditions
        let mut reserved = self.reserved.write().await;
        
        // Check available balance while holding the lock
        let available = self.get_available_balance_locked(mint, &reserved).await?;

        if available < amount {
            return Err(anyhow::anyhow!(
                "Insufficient balance for mint {}: need {}, available {}",
                mint,
                amount,
                available
            ));
        }

        // Reserve the amount atomically (add to existing reserved amount)
        *reserved.entry(*mint).or_insert(0) += amount;
        
        // Lock is released here, but reservation is already committed
        Ok(ReservationGuard {
            reserved: Arc::clone(&self.reserved),
            mint: *mint,
            amount,
        })
    }

    pub async fn release(&self, mint: &Pubkey, amount: u64) {
        let mut reserved = self.reserved.write().await;
        if let Some(reserved_amount) = reserved.get_mut(mint) {
            *reserved_amount = reserved_amount.saturating_sub(amount);
            if *reserved_amount == 0 {
                reserved.remove(mint);
            }
        }
    }
}

pub struct ReservationGuard {
    reserved: Arc<RwLock<HashMap<Pubkey, u64>>>,
    mint: Pubkey,
    amount: u64,
}

impl Drop for ReservationGuard {
    fn drop(&mut self) {
        let reserved = Arc::clone(&self.reserved);
        let mint = self.mint;
        let amount = self.amount;
        tokio::spawn(async move {
            let mut reserved = reserved.write().await;
            if let Some(reserved_amount) = reserved.get_mut(&mint) {
                *reserved_amount = reserved_amount.saturating_sub(amount);
                if *reserved_amount == 0 {
                    reserved.remove(&mint);
                }
            }
        });
    }
}
