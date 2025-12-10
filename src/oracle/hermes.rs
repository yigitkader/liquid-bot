// Pyth Hermes API Client Module
// Dynamically discovers Pyth feed IDs and prices from Hermes API
// This eliminates the need for hardcoded/static feed addresses

use anyhow::Result;
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use simple_pyth_client_rs::{get_price_feeds, get_token_price_info};

/// Cache for discovered Pyth feed IDs
/// Key: token symbol (e.g., "SOL", "BTC", "ETH")
/// Value: CachedFeed with feed_id and metadata
struct FeedCache {
    feeds: HashMap<String, CachedFeed>,
    all_feeds_loaded: bool,
    last_refresh: Option<Instant>,
}

#[derive(Clone)]
struct CachedFeed {
    feed_id: String,        // Hex feed ID (e.g., "ef0d8b6f...")
    description: String,    // Feed description
    base: String,           // Base currency (e.g., "SOL")
    quote: String,          // Quote currency (e.g., "USD")
    cached_at: Instant,
}

lazy_static::lazy_static! {
    static ref FEED_CACHE: RwLock<FeedCache> = RwLock::new(FeedCache {
        feeds: HashMap::new(),
        all_feeds_loaded: false,
        last_refresh: None,
    });
}

/// Cache TTL for feed discovery (1 hour - feeds don't change often)
const FEED_CACHE_TTL_SECS: u64 = 3600;

/// Load all Pyth feeds from Hermes API and cache them
/// This is called once and caches all available feeds
async fn load_all_feeds() -> Result<()> {
    {
        let cache = FEED_CACHE.read().unwrap();
        if cache.all_feeds_loaded {
            if let Some(last_refresh) = cache.last_refresh {
                if last_refresh.elapsed().as_secs() < FEED_CACHE_TTL_SECS {
                    return Ok(()); // Cache is fresh
                }
            }
        }
    }
    
    log::info!("🔍 Loading all Pyth feeds from Hermes API...");
    
    // get_price_feeds() returns Vec<PriceFeed> with id and attributes
    let feeds = match get_price_feeds().await {
        Ok(feeds) => feeds,
        Err(e) => {
            log::warn!("⚠️  Hermes API get_price_feeds failed: {}", e);
            return Err(anyhow::anyhow!("Hermes API failed: {}", e));
        }
    };
    
    log::info!("📊 Loaded {} Pyth feeds from Hermes API", feeds.len());
    
    // Cache all USD feeds
    let mut cache = FEED_CACHE.write().unwrap();
    let mut usd_count = 0;
    
    for feed in feeds {
        // Only cache USD pairs (most useful for liquidation)
        if feed.attributes.quote_currency.to_uppercase() == "USD" {
            let base_upper = feed.attributes.base.to_uppercase();
            
            // Don't overwrite if already exists
            if !cache.feeds.contains_key(&base_upper) {
                cache.feeds.insert(base_upper.clone(), CachedFeed {
                    feed_id: feed.id.clone(),
                    description: feed.attributes.description.clone(),
                    base: feed.attributes.base.clone(),
                    quote: feed.attributes.quote_currency.clone(),
                    cached_at: Instant::now(),
                });
                usd_count += 1;
            }
        }
    }
    
    cache.all_feeds_loaded = true;
    cache.last_refresh = Some(Instant::now());
    
    log::info!("✅ Cached {} USD price feeds from Pyth", usd_count);
    
    Ok(())
}

/// Discover Pyth feed ID for a token symbol from Hermes API
/// This is the recommended way to get feed IDs - dynamic discovery at runtime
/// 
/// # Arguments
/// * `symbol` - Token symbol (e.g., "SOL", "BTC", "ETH")
/// 
/// # Returns
/// * `Ok(Some(feed_id))` - Feed ID found (hex string)
/// * `Ok(None)` - No feed found for this symbol
/// * `Err(e)` - API error
pub async fn discover_feed_id(symbol: &str) -> Result<Option<String>> {
    let symbol_upper = symbol.to_uppercase();
    
    // Check cache first
    {
        let cache = FEED_CACHE.read().unwrap();
        if let Some(cached) = cache.feeds.get(&symbol_upper) {
            if cached.cached_at.elapsed().as_secs() < FEED_CACHE_TTL_SECS {
                log::debug!(
                    "✅ Using cached Pyth feed for {}: {} ({})",
                    symbol_upper,
                    cached.feed_id,
                    cached.description
                );
                return Ok(Some(cached.feed_id.clone()));
            }
        }
    }
    
    // Load all feeds if not already loaded
    load_all_feeds().await?;
    
    // Check cache again after loading
    {
        let cache = FEED_CACHE.read().unwrap();
        if let Some(cached) = cache.feeds.get(&symbol_upper) {
            log::info!(
                "✅ Discovered Pyth feed for {}/USD: {} ({})",
                symbol_upper,
                cached.feed_id,
                cached.description
            );
            return Ok(Some(cached.feed_id.clone()));
        }
    }
    
    log::warn!("⚠️  No Pyth feed found for symbol: {}", symbol_upper);
    Ok(None)
}

/// Get current price for a token from Hermes API
/// This is a fallback when on-chain oracles fail
/// 
/// # Arguments
/// * `symbol` - Token symbol (e.g., "SOL", "BTC", "ETH")
/// 
/// # Returns
/// * `Ok(Some(price))` - Price in USD
/// * `Ok(None)` - No price available
/// * `Err(e)` - API error
pub async fn get_price_from_hermes(symbol: &str) -> Result<Option<f64>> {
    let symbol_upper = symbol.to_uppercase();
    
    // First, ensure we have the feed ID
    let feed_id = match discover_feed_id(&symbol_upper).await? {
        Some(id) => id,
        None => {
            log::warn!("⚠️  Cannot get price: no feed ID for {}", symbol_upper);
            return Ok(None);
        }
    };
    
    // Get price from Hermes API using get_token_price_info
    log::debug!("🔍 Fetching price for {} from Hermes API (feed: {})", symbol_upper, feed_id);
    
    // get_token_price_info takes &Vec<String> and returns Vec<TokenPriceInfo>
    let feed_ids = vec![feed_id.clone()];
    let prices = match get_token_price_info(&feed_ids).await {
        Ok(prices) => prices,
        Err(e) => {
            log::warn!("⚠️  Hermes API price fetch failed for {}: {}", symbol_upper, e);
            return Err(anyhow::anyhow!("Hermes API price fetch failed: {}", e));
        }
    };
    
    if let Some(price_info) = prices.first() {
        let price = price_info.price_30s; // Use 30-second price (more stable)
        
        if price > 0.0 {
            log::info!(
                "✅ Price from Hermes API for {}: ${:.4} (feed: {})",
                symbol_upper,
                price,
                feed_id
            );
            return Ok(Some(price));
        }
    }
    
    log::warn!("⚠️  Hermes API returned no valid price for {}", symbol_upper);
    Ok(None)
}

/// Get prices for multiple tokens at once
/// More efficient than calling get_price_from_hermes for each token
pub async fn get_prices_batch(symbols: &[&str]) -> Result<HashMap<String, f64>> {
    let mut result = HashMap::new();
    let mut feed_ids_to_fetch = Vec::new();
    let mut symbol_to_feed: HashMap<String, String> = HashMap::new();
    
    // Get feed IDs for all symbols
    for symbol in symbols {
        let symbol_upper = symbol.to_uppercase();
        if let Some(feed_id) = discover_feed_id(&symbol_upper).await? {
            feed_ids_to_fetch.push(feed_id.clone());
            symbol_to_feed.insert(feed_id, symbol_upper);
        }
    }
    
    if feed_ids_to_fetch.is_empty() {
        return Ok(result);
    }
    
    // Fetch all prices at once
    let prices = match get_token_price_info(&feed_ids_to_fetch).await {
        Ok(prices) => prices,
        Err(e) => {
            log::warn!("⚠️  Hermes API batch price fetch failed: {}", e);
            return Err(anyhow::anyhow!("Hermes API batch price fetch failed: {}", e));
        }
    };
    
    // Map prices back to symbols
    for price_info in prices {
        if let Some(symbol) = symbol_to_feed.get(&price_info.token_id) {
            if price_info.price_30s > 0.0 {
                result.insert(symbol.clone(), price_info.price_30s);
            }
        }
    }
    
    Ok(result)
}

/// Initialize Pyth feeds at startup
/// Call this once at bot startup to pre-warm the cache
pub async fn initialize_pyth_feeds() -> Result<()> {
    log::info!("🚀 Initializing Pyth Hermes feed discovery...");
    
    match load_all_feeds().await {
        Ok(()) => {
            let (count, _) = get_cache_stats();
            log::info!("✅ Pyth feed discovery initialized: {} USD feeds cached", count);
            
            // Log some common feeds for verification
            let common = ["SOL", "BTC", "ETH", "USDC"];
            for symbol in common {
                if let Ok(Some(feed_id)) = discover_feed_id(symbol).await {
                    log::info!("  - {}/USD: {}", symbol, feed_id);
                }
            }
            
            Ok(())
        }
        Err(e) => {
            log::warn!(
                "⚠️  Pyth feed discovery failed (will retry on demand): {}",
                e
            );
            // Don't fail startup - feeds can be discovered on-demand
            Ok(())
        }
    }
}

/// Clear the feed cache (useful for testing or when feeds need to be refreshed)
pub fn clear_feed_cache() {
    let mut cache = FEED_CACHE.write().unwrap();
    cache.feeds.clear();
    cache.all_feeds_loaded = false;
    cache.last_refresh = None;
    log::info!("🗑️  Pyth feed cache cleared");
}

/// Get statistics about the feed cache
pub fn get_cache_stats() -> (usize, Option<Duration>) {
    let cache = FEED_CACHE.read().unwrap();
    let age = cache.last_refresh.map(|t| t.elapsed());
    (cache.feeds.len(), age)
}

/// Get the cached feed ID for a symbol without making API calls
/// Returns None if not in cache
pub fn get_cached_feed_id(symbol: &str) -> Option<String> {
    let symbol_upper = symbol.to_uppercase();
    let cache = FEED_CACHE.read().unwrap();
    cache.feeds.get(&symbol_upper).map(|f| f.feed_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    
    // Note: These tests require network access
    // Run with: cargo test -- --ignored
    
    #[tokio::test]
    #[ignore] // Requires network
    async fn test_discover_sol_feed() {
        let feed_id = discover_feed_id("SOL").await.unwrap();
        assert!(feed_id.is_some());
        let feed_id = feed_id.unwrap();
        // SOL/USD feed ID should be this
        assert_eq!(feed_id, "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d");
        println!("SOL feed ID: {}", feed_id);
    }
    
    #[tokio::test]
    #[ignore] // Requires network
    async fn test_get_sol_price() {
        let price = get_price_from_hermes("SOL").await.unwrap();
        assert!(price.is_some());
        let price = price.unwrap();
        assert!(price > 0.0);
        println!("SOL price: ${:.2}", price);
    }
    
    #[tokio::test]
    #[ignore] // Requires network
    async fn test_initialize_feeds() {
        initialize_pyth_feeds().await.unwrap();
        let (count, _) = get_cache_stats();
        assert!(count > 0);
        println!("Cached {} feeds", count);
    }
}
