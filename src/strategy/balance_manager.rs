use crate::blockchain::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use anyhow::Result;

pub struct BalanceManager {
    reserved: Arc<RwLock<HashMap<Pubkey, u64>>>,
    rpc: Arc<RpcClient>,
    wallet: Pubkey,
}

impl BalanceManager {
    pub fn new(rpc: Arc<RpcClient>, wallet: Pubkey) -> Self {
        BalanceManager {
            reserved: Arc::new(RwLock::new(HashMap::new())),
            rpc,
            wallet,
        }
    }

    pub async fn get_available_balance(&self, mint: &Pubkey) -> Result<u64> {
        // Must get real token balance from RPC - cannot use placeholder
        // Get associated token account for wallet and mint
        use crate::protocol::solend::accounts::get_associated_token_address;
        let ata = get_associated_token_address(&self.wallet, mint, None)?;
        
        // Get token account data from RPC
        let account = self.rpc.get_account(&ata).await?;
        
        // Parse token account to get balance
        // Token account structure: mint (32) + owner (32) + amount (8) + ...
        if account.data.len() < 72 {
            return Err(anyhow::anyhow!("Invalid token account data"));
        }
        
        // Read balance from bytes 64-72 (u64 little-endian)
        let balance_bytes: [u8; 8] = account.data[64..72].try_into()
            .map_err(|_| anyhow::anyhow!("Failed to read balance from token account"))?;
        let actual = u64::from_le_bytes(balance_bytes);
        
        let reserved = self.reserved.read().await.get(mint).copied().unwrap_or(0);
        let available = actual.saturating_sub(reserved);
        Ok(available)
    }

    pub async fn reserve(&self, mint: &Pubkey, amount: u64) -> Result<ReservationGuard> {
        let available = self.get_available_balance(mint).await?;

        if available < amount {
            return Err(anyhow::anyhow!("Insufficient balance"));
        }

        self.reserved.write().await.insert(*mint, amount);

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
