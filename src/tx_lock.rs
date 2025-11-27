use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

pub struct TxLock {
    locked_accounts: Arc<RwLock<HashSet<String>>>,
    lock_duration: Duration,
}

impl TxLock {
    pub fn new(lock_duration_secs: u64) -> Self {
        TxLock {
            locked_accounts: Arc::new(RwLock::new(HashSet::new())),
            lock_duration: Duration::from_secs(lock_duration_secs),
        }
    }

    pub async fn try_lock(&self, account_address: &str) -> bool {
        let mut locked = self.locked_accounts.write().await;
        if locked.contains(account_address) {
            false
        } else {
            locked.insert(account_address.to_string());
            true
        }
    }

    pub async fn unlock(&self, account_address: &str) {
        let mut locked = self.locked_accounts.write().await;
        locked.remove(account_address);
    }

    pub async fn is_locked(&self, account_address: &str) -> bool {
        let locked = self.locked_accounts.read().await;
        locked.contains(account_address)
    }
}
