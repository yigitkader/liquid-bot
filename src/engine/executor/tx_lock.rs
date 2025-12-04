/// Transaction lock mechanism to prevent double liquidation
/// 
/// This module provides a locking mechanism to ensure that the same position
/// is not liquidated multiple times concurrently.

use anyhow::Result;
use solana_sdk::pubkey::Pubkey;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock as TokioRwLock;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;

/// Transaction lock to prevent concurrent liquidation of the same position
pub struct TxLock {
    locked: Arc<std::sync::RwLock<HashSet<Pubkey>>>,
    lock_times: Arc<std::sync::RwLock<HashMap<Pubkey, std::time::Instant>>>,
    timeout_seconds: u64,
    cancel_token: CancellationToken,
}

impl TxLock {
    pub fn new(timeout_seconds: u64) -> Self {
        TxLock {
            locked: Arc::new(std::sync::RwLock::new(HashSet::new())),
            lock_times: Arc::new(std::sync::RwLock::new(HashMap::new())),
            timeout_seconds,
            cancel_token: CancellationToken::new(),
        }
    }

    /// Start background cleanup task to remove expired locks
    pub fn start_cleanup_task(self: &Arc<Self>) {
        let locked = Arc::clone(&self.locked);
        let lock_times = Arc::clone(&self.lock_times);
        let timeout_seconds = self.timeout_seconds;
        let cancel = self.cancel_token.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = sleep(Duration::from_secs(10)) => {
                        // Run cleanup every 10 seconds
                        // Handle poisoned locks gracefully (self-healing)
                        if let Ok(mut locked_guard) = locked.write() {
                            if let Ok(mut lock_times_guard) = lock_times.write() {
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
                                    log::debug!(
                                        "TxLock: cleaned up {} expired lock(s)",
                                        expired_addresses.len()
                                    );
                                }
                            } else {
                                log::warn!("TxLock: lock_times RwLock is poisoned, skipping cleanup cycle (self-healing)");
                            }
                        } else {
                            log::warn!("TxLock: locked RwLock is poisoned, skipping cleanup cycle (self-healing)");
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

    /// Try to acquire a lock for the given address
    /// 
    /// Returns a guard that will release the lock when dropped.
    /// Returns an error if the address is already locked.
    pub fn try_lock(&self, address: &Pubkey) -> Result<TxLockGuard> {
        // Handle poisoned locks gracefully (self-healing)
        let mut locked = self.locked.write()
            .map_err(|_| anyhow::anyhow!("Lock is poisoned (previous panic detected) - cannot acquire lock"))?;
        let mut lock_times = self.lock_times.write()
            .map_err(|_| anyhow::anyhow!("Lock times is poisoned (previous panic detected) - cannot acquire lock"))?;

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

/// Guard that releases the lock when dropped
pub struct TxLockGuard {
    locked: Arc<std::sync::RwLock<HashSet<Pubkey>>>,
    lock_times: Arc<std::sync::RwLock<HashMap<Pubkey, std::time::Instant>>>,
    address: Pubkey,
}

impl Drop for TxLockGuard {
    fn drop(&mut self) {
        // Handle poisoned locks gracefully (self-healing)
        // This is especially important in Drop, as panics in Drop are double-panic (fatal)
        if let Ok(mut locked) = self.locked.write() {
            locked.remove(&self.address);
        } else {
            log::warn!("TxLockGuard: locked RwLock is poisoned during drop, skipping cleanup (self-healing)");
        }
        
        if let Ok(mut lock_times) = self.lock_times.write() {
            lock_times.remove(&self.address);
        } else {
            log::warn!("TxLockGuard: lock_times RwLock is poisoned during drop, skipping cleanup (self-healing)");
        }
    }
}

