use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::time::Duration;

/// TX-lock mekanizması - Aynı pozisyonun tekrar işlenmesini önler
pub struct TxLock {
    /// İşlenmekte olan account'lar (account_address -> lock_time)
    locked_accounts: Arc<RwLock<HashSet<String>>>,
    /// Lock süresi (saniye)
    lock_duration: Duration,
}

impl TxLock {
    pub fn new(lock_duration_secs: u64) -> Self {
        TxLock {
            locked_accounts: Arc::new(RwLock::new(HashSet::new())),
            lock_duration: Duration::from_secs(lock_duration_secs),
        }
    }

    /// Account'u kilitle (işlenmekte olarak işaretle)
    /// Eğer zaten kilitliyse false döner
    pub async fn try_lock(&self, account_address: &str) -> bool {
        let mut locked = self.locked_accounts.write().await;
        if locked.contains(account_address) {
            false
        } else {
            locked.insert(account_address.to_string());
            true
        }
    }

    /// Account kilidini kaldır
    pub async fn unlock(&self, account_address: &str) {
        let mut locked = self.locked_accounts.write().await;
        locked.remove(account_address);
    }

    /// Account'un kilitli olup olmadığını kontrol et
    pub async fn is_locked(&self, account_address: &str) -> bool {
        let locked = self.locked_accounts.read().await;
        locked.contains(account_address)
    }
}

