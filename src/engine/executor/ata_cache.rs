// ATA (Associated Token Account) cache for executor

use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;

/// ATA cache entry with metadata
#[derive(Clone, Debug)]
pub struct AtaCacheEntry {
    pub created_at: Instant,
    pub last_accessed: Instant,
    pub verified: bool,
}

/// ATA cache with TTL and size limits
pub struct AtaCache {
    cache: Arc<RwLock<HashMap<Pubkey, AtaCacheEntry>>>,
    cleanup_token: CancellationToken,
}

impl AtaCache {
    const TTL_SECONDS: u64 = 1 * 3600; // 1 hour
    const MAX_SIZE: usize = 50000;
    const CLEANUP_INTERVAL_SECONDS: u64 = 300; // 5 minutes

    pub fn new() -> (Self, CancellationToken) {
        let cache: Arc<RwLock<HashMap<Pubkey, AtaCacheEntry>>> = Arc::new(RwLock::new(HashMap::new()));
        let cleanup_token = CancellationToken::new();
        
        let cache_for_cleanup = Arc::clone(&cache);
        let cleanup_token_clone = cleanup_token.clone();
        
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = sleep(Duration::from_secs(Self::CLEANUP_INTERVAL_SECONDS)) => {
                        let mut cache_guard = cache_for_cleanup.write().await;
                        let now = Instant::now();
                        let before_len = cache_guard.len();
                        
                        // Remove expired entries
                        cache_guard.retain(|_, entry| {
                            now.duration_since(entry.created_at) < Duration::from_secs(Self::TTL_SECONDS)
                        });
                        
                        // If still too large, remove least recently used
                        if cache_guard.len() > Self::MAX_SIZE {
                            let mut entries: Vec<(Pubkey, AtaCacheEntry)> = cache_guard.drain().collect();
                            entries.sort_by(|a, b| a.1.last_accessed.cmp(&b.1.last_accessed));
                            let to_remove = entries.len().saturating_sub(Self::MAX_SIZE);
                            let removed_count = to_remove;
                            
                            for (pubkey, entry) in entries.into_iter().skip(to_remove) {
                                cache_guard.insert(pubkey, entry);
                            }
                            
                            log::warn!(
                                "Executor: ATA cache exceeded max size ({}), removed {} least-recently-used entries (now: {})",
                                Self::MAX_SIZE,
                                removed_count,
                                cache_guard.len()
                            );
                        }
                        
                        let after_len = cache_guard.len();
                        if before_len != after_len {
                            log::debug!(
                                "Executor: ATA cache cleanup: removed {} entries ({} remaining)",
                                before_len - after_len,
                                after_len
                            );
                        }
                    }
                    _ = cleanup_token_clone.cancelled() => {
                        log::info!("Executor ATA cache cleanup task shutting down gracefully");
                        break;
                    }
                }
            }
        });

        (Self {
            cache,
            cleanup_token: cleanup_token.clone(),
        }, cleanup_token)
    }

    pub async fn get(&self, pubkey: &Pubkey) -> Option<AtaCacheEntry> {
        let mut cache = self.cache.write().await;
        if let Some(entry) = cache.get_mut(pubkey) {
            entry.last_accessed = Instant::now();
            Some(entry.clone())
        } else {
            None
        }
    }

    pub async fn insert(&self, pubkey: Pubkey, verified: bool) {
        let mut cache = self.cache.write().await;
        let now = Instant::now();
        cache.insert(pubkey, AtaCacheEntry {
            created_at: now,
            last_accessed: now,
            verified,
        });
    }

    pub async fn contains(&self, pubkey: &Pubkey) -> bool {
        let cache = self.cache.read().await;
        cache.contains_key(pubkey)
    }

    pub fn cancel_cleanup(&self) {
        self.cleanup_token.cancel();
    }

    pub fn cache(&self) -> Arc<RwLock<HashMap<Pubkey, AtaCacheEntry>>> {
        Arc::clone(&self.cache)
    }
}

