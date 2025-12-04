use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Reservation manager for tracking reserved token amounts
pub struct ReservationManager {
    reserved: Arc<RwLock<HashMap<Pubkey, u64>>>, // mint -> reserved amount
}

impl ReservationManager {
    pub fn new() -> Self {
        Self {
            reserved: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn get(&self, mint: &Pubkey) -> u64 {
        let reserved = self.reserved.read().await;
        reserved.get(mint).copied().unwrap_or(0)
    }

    pub async fn reserve(&self, mint: &Pubkey, amount: u64) {
        let mut reserved = self.reserved.write().await;
        let current = reserved.get(mint).copied().unwrap_or(0);
        reserved.insert(*mint, current.saturating_add(amount));
    }

    pub async fn release(&self, mint: &Pubkey, amount: u64) {
        let mut reserved = self.reserved.write().await;
        if let Some(current) = reserved.get_mut(mint) {
            *current = current.saturating_sub(amount);
            if *current == 0 {
                reserved.remove(mint);
            }
        }
    }

    pub async fn set(&self, mint: &Pubkey, amount: u64) {
        let mut reserved = self.reserved.write().await;
        if amount == 0 {
            reserved.remove(mint);
        } else {
            reserved.insert(*mint, amount);
        }
    }

    pub fn reserved(&self) -> Arc<RwLock<HashMap<Pubkey, u64>>> {
        Arc::clone(&self.reserved)
    }
}

impl Default for ReservationManager {
    fn default() -> Self {
        Self::new()
    }
}

