use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Cached balance with timestamp for freshness checking
#[derive(Clone, Debug)]
pub struct CachedBalance {
    pub amount: u64,
    pub timestamp: Instant,
}

/// Cache time-to-live: balances older than this will be considered stale
pub const CACHE_TTL: Duration = Duration::from_secs(60);

/// Balance cache for storing token balances with TTL
pub struct BalanceCache {
    balances: Arc<RwLock<HashMap<Pubkey, CachedBalance>>>,
}

impl BalanceCache {
    pub fn new() -> Self {
        Self {
            balances: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn get(&self, ata: &Pubkey) -> Option<CachedBalance> {
        let balances = self.balances.read().await;
        balances.get(ata).cloned()
    }

    pub async fn get_fresh(&self, ata: &Pubkey) -> Option<u64> {
        let balances = self.balances.read().await;
        if let Some(cached) = balances.get(ata) {
            if cached.timestamp.elapsed() < CACHE_TTL {
                return Some(cached.amount);
            }
        }
        None
    }

    pub async fn insert(&self, ata: Pubkey, amount: u64) {
        let mut balances = self.balances.write().await;
        balances.insert(
            ata,
            CachedBalance {
                amount,
                timestamp: Instant::now(),
            },
        );
    }

    pub async fn update(&self, ata: &Pubkey, amount: u64) {
        let mut balances = self.balances.write().await;
        if let Some(cached) = balances.get_mut(ata) {
            cached.amount = amount;
            cached.timestamp = Instant::now();
        } else {
            balances.insert(
                *ata,
                CachedBalance {
                    amount,
                    timestamp: Instant::now(),
                },
            );
        }
    }

    pub fn balances(&self) -> Arc<RwLock<HashMap<Pubkey, CachedBalance>>> {
        Arc::clone(&self.balances)
    }
}

impl Default for BalanceCache {
    fn default() -> Self {
        Self::new()
    }
}

