// ATA (Associated Token Account) cache for executor

use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;
use lru::LruCache;

/// ATA cache entry with metadata
#[derive(Clone, Debug)]
pub struct AtaCacheEntry {
    pub created_at: Instant,
    pub last_accessed: Instant,
    pub verified: bool,
}

/// ATA cache with TTL and size limits
/// 
/// Uses LRU cache for O(1) insert/get/eviction operations
/// Previous implementation used O(n log n) sort for eviction, which was slow at scale
pub struct AtaCache {
    cache: Arc<RwLock<LruCache<Pubkey, AtaCacheEntry>>>,
    cleanup_token: CancellationToken,
}

impl AtaCache {
    const TTL_SECONDS: u64 = 1 * 3600; // 1 hour
    const MAX_SIZE: usize = 50000;
    const CLEANUP_INTERVAL_SECONDS: u64 = 300; // 5 minutes

    pub fn new() -> (Self, CancellationToken) {
        // ✅ FIX: Use LRU cache for O(1) operations instead of O(n log n) sort
        // Problem: Previous implementation used HashMap + sort() for eviction
        // - MAX_SIZE = 50,000
        // - sort(): O(50,000 log 50,000) = ~800,000 operations
        // - Cleanup interval: 5 minutes
        // - Every cleanup: 800k operations → potential lag
        // Solution: Use LruCache which provides O(1) insert/get/eviction
        // - Automatic eviction when max size reached
        // - No need for manual sorting
        // - Much better performance at scale
        use std::num::NonZeroUsize;
        let max_size = NonZeroUsize::new(Self::MAX_SIZE).unwrap_or(NonZeroUsize::new(50000).unwrap());
        let cache: Arc<RwLock<LruCache<Pubkey, AtaCacheEntry>>> = Arc::new(
            RwLock::new(LruCache::new(max_size))
        );
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
                        
                        // ✅ FIX: Remove expired entries efficiently
                        // LRU cache doesn't support retain(), so we need to iterate and remove
                        // But this is still O(n) instead of O(n log n) sort
                        let mut keys_to_remove: Vec<Pubkey> = Vec::new();
                        for (pubkey, entry) in cache_guard.iter() {
                            if now.duration_since(entry.created_at) >= Duration::from_secs(Self::TTL_SECONDS) {
                                keys_to_remove.push(*pubkey);
                            }
                        }
                        
                        // Remove expired entries
                        for key in keys_to_remove {
                            cache_guard.pop(&key);
                        }
                        
                        // ✅ LRU cache automatically evicts when max size is reached
                        // No need for manual sorting - eviction happens on insert
                        let after_len = cache_guard.len();
                        if before_len != after_len {
                            log::debug!(
                                "Executor: ATA cache cleanup: removed {} expired entries ({} remaining, max: {})",
                                before_len - after_len,
                                after_len,
                                Self::MAX_SIZE
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
        // ✅ LRU cache automatically updates access order on get()
        if let Some(entry) = cache.get(pubkey) {
            let mut updated_entry = entry.clone();
            updated_entry.last_accessed = Instant::now();
            // Re-insert to update access time (LRU maintains order)
            cache.put(*pubkey, updated_entry.clone());
            Some(updated_entry)
        } else {
            None
        }
    }

    pub async fn insert(&self, pubkey: Pubkey, verified: bool) {
        let mut cache = self.cache.write().await;
        let now = Instant::now();
        // ✅ LRU cache automatically evicts least-recently-used when max size reached
        // This is O(1) operation, much faster than previous O(n log n) sort
        cache.put(pubkey, AtaCacheEntry {
            created_at: now,
            last_accessed: now,
            verified,
        });
    }

    pub async fn contains(&self, pubkey: &Pubkey) -> bool {
        let cache = self.cache.read().await;
        cache.contains(pubkey)
    }

    pub fn cancel_cleanup(&self) {
        self.cleanup_token.cancel();
    }

    pub fn cache(&self) -> Arc<RwLock<LruCache<Pubkey, AtaCacheEntry>>> {
        Arc::clone(&self.cache)
    }
}

