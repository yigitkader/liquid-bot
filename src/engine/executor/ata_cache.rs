/// ATA (Associated Token Account) cache management
/// 
/// This module provides caching for ATA verification to avoid redundant
/// RPC calls and instruction creation.

use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;

/// ATA cache entry with timestamps for LRU eviction
#[derive(Clone, Debug)]
pub struct AtaCacheEntry {
    pub created_at: Instant,
    pub last_accessed: Instant,
    pub verified: bool,
}

/// ATA cache with LRU eviction
pub struct AtaCache {
    cache: Arc<RwLock<HashMap<Pubkey, AtaCacheEntry>>>,
    cleanup_token: CancellationToken,
}

impl AtaCache {
    const TTL_SECONDS: u64 = 1 * 3600; // 1 hour
    const MAX_SIZE: usize = 50000; // 50k entries
    const CLEANUP_INTERVAL_SECONDS: u64 = 300; // 5 minutes

    pub fn new() -> Self {
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
                        
                        // Strategy 1: Remove entries older than TTL
                        cache_guard.retain(|_, entry| {
                            now.duration_since(entry.created_at) < Duration::from_secs(Self::TTL_SECONDS)
                        });
                        
                        // Strategy 2: If cache is still too large, use LRU eviction
                        if cache_guard.len() > Self::MAX_SIZE {
                            let mut entries: Vec<(Pubkey, AtaCacheEntry)> = cache_guard.drain().collect();
                            // Sort by last_accessed (least-recently-used first)
                            entries.sort_by(|a, b| a.1.last_accessed.cmp(&b.1.last_accessed));
                            // Keep only the most-recently-used entries
                            let to_remove = entries.len().saturating_sub(Self::MAX_SIZE);
                            let removed_count = to_remove;
                            // Insert back only the most-recently-used entries
                            for (pubkey, entry) in entries.into_iter().skip(to_remove) {
                                cache_guard.insert(pubkey, entry);
                            }
                            
                            log::warn!(
                                "AtaCache: Cache exceeded max size ({}), removed {} least-recently-used entries (now: {})",
                                Self::MAX_SIZE,
                                removed_count,
                                cache_guard.len()
                            );
                        }
                        
                        let after_len = cache_guard.len();
                        
                        if before_len != after_len {
                            log::debug!(
                                "AtaCache: Cleanup removed {} entries ({} remaining, TTL: {}h, max_size: {}, LRU eviction)",
                                before_len - after_len,
                                after_len,
                                Self::TTL_SECONDS / 3600,
                                Self::MAX_SIZE
                            );
                        }
                    }
                    _ = cleanup_token_clone.cancelled() => {
                        log::info!("AtaCache cleanup task shutting down gracefully");
                        break;
                    }
                }
            }
        });

        AtaCache {
            cache,
            cleanup_token,
        }
    }

    /// Get cache reference for read/write operations
    pub fn cache(&self) -> Arc<RwLock<HashMap<Pubkey, AtaCacheEntry>>> {
        Arc::clone(&self.cache)
    }

    /// Stop cleanup task
    pub fn stop(&self) {
        self.cleanup_token.cancel();
    }

    /// Evict LRU entries when cache is full (immediate eviction)
    pub fn evict_lru_if_needed(cache: &mut HashMap<Pubkey, AtaCacheEntry>) {
        if cache.len() > Self::MAX_SIZE {
            let mut entries: Vec<(Pubkey, AtaCacheEntry)> = cache.drain().collect();
            entries.sort_by(|a, b| a.1.last_accessed.cmp(&b.1.last_accessed));
            let to_remove = entries.len().saturating_sub(Self::MAX_SIZE);
            let removed_count = to_remove;
            
            for (pubkey, entry) in entries.into_iter().skip(to_remove) {
                cache.insert(pubkey, entry);
            }
            
            log::warn!(
                "AtaCache: Immediate LRU eviction: removed {} entries (now: {})",
                removed_count,
                cache.len()
            );
        }
    }
}

