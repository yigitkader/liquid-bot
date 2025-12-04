// Transaction lock mechanism - prevents concurrent transactions on same account

use anyhow::Result;
use solana_sdk::pubkey::Pubkey;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;

pub struct TxLock {
    locked: Arc<RwLock<HashSet<Pubkey>>>,
    lock_times: Arc<RwLock<HashMap<Pubkey, Instant>>>,
    timeout_seconds: u64,
    cancel_token: CancellationToken,
}

impl TxLock {
    pub fn new(timeout_seconds: u64) -> Self {
        TxLock {
            locked: Arc::new(RwLock::new(HashSet::new())),
            lock_times: Arc::new(RwLock::new(HashMap::new())),
            timeout_seconds,
            cancel_token: CancellationToken::new(),
        }
    }

    pub fn start_cleanup_task(self: &Arc<Self>) {
        let locked = Arc::clone(&self.locked);
        let lock_times = Arc::clone(&self.lock_times);
        let timeout_seconds = self.timeout_seconds;
        let cancel = self.cancel_token.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = sleep(Duration::from_secs(10)) => {
                        let mut locked_guard = locked.write().await;
                        let mut lock_times_guard = lock_times.write().await;
                                // Remove expired locks
                                let expired_addresses: Vec<Pubkey> = lock_times_guard
                                    .iter()
                                    .filter_map(|(address, time)| {
                                        if time.elapsed().as_secs() >= timeout_seconds {
                                            Some(*address)
                                        } else {
                                            None
                                        }
                                    })
                                    .collect();

                                for address in &expired_addresses {
                                    locked_guard.remove(address);
                                    lock_times_guard.remove(address);
                                }

                        if !expired_addresses.is_empty() {
                            log::debug!("TxLock: cleaned up {} expired lock(s)", expired_addresses.len());
                        }
                    }
                    _ = cancel.cancelled() => {
                        log::info!("TxLock cleanup task shutting down gracefully");
                        break;
                    }
                }
            }
        });
    }

    pub fn cancel_cleanup(&self) {
        self.cancel_token.cancel();
    }

    pub async fn try_lock(&self, address: &Pubkey) -> Result<TxLockGuard> {
        let mut locked = self.locked.write().await;
        let mut lock_times = self.lock_times.write().await;

        if let Some(lock_time) = lock_times.get(address) {
            if lock_time.elapsed().as_secs() >= self.timeout_seconds {
                locked.remove(address);
                lock_times.remove(address);
            } else {
                return Err(anyhow::anyhow!("Account already locked"));
            }
        }

        locked.insert(*address);
        lock_times.insert(*address, std::time::Instant::now());

        Ok(TxLockGuard {
            locked: Arc::clone(&self.locked),
            lock_times: Arc::clone(&self.lock_times),
            address: *address,
        })
    }
}

pub struct TxLockGuard {
    locked: Arc<RwLock<HashSet<Pubkey>>>,
    lock_times: Arc<RwLock<HashMap<Pubkey, Instant>>>,
    address: Pubkey,
}

impl Drop for TxLockGuard {
    fn drop(&mut self) {
        // Drop must be synchronous, so we use blocking write()
        // This is acceptable because:
        // - The operation is very fast (just HashSet/HashMap remove)
        // - Happens during cleanup, not in hot path
        // - Alternative (async Drop) doesn't exist in Rust
        
        // Use try_write() to avoid blocking if possible
        // If it fails, spawn a task to clean up asynchronously
        if let Ok(mut locked_guard) = self.locked.try_write() {
            locked_guard.remove(&self.address);
        } else {
            // Lock is held, spawn async task to clean up later
            let locked = Arc::clone(&self.locked);
            let address = self.address;
            tokio::spawn(async move {
                let mut locked_guard = locked.write().await;
                locked_guard.remove(&address);
            });
        }
        
        if let Ok(mut lock_times_guard) = self.lock_times.try_write() {
            lock_times_guard.remove(&self.address);
        } else {
            // Lock is held, spawn async task to clean up later
            let lock_times = Arc::clone(&self.lock_times);
            let address = self.address;
            tokio::spawn(async move {
                let mut lock_times_guard = lock_times.write().await;
                lock_times_guard.remove(&address);
            });
        }
    }
}

