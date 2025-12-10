// Oracle module - coordinates between Pyth and Switchboard oracles
// Separated from pipeline.rs to reduce code size and improve maintainability

pub mod pyth;
pub mod switchboard;
pub mod validation;
pub mod hermes; // Pyth Hermes API - dynamic feed discovery

use anyhow::Result;
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::RwLock;
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[allow(dead_code)]

/// Simple price cache entry (1 second TTL)
struct CachedPrice {
    price: f64,
    timestamp: Instant,
    has_pyth: bool,
    has_switchboard: bool,
}

/// Global price cache (1 second TTL per oracle)
static PRICE_CACHE: OnceLock<RwLock<HashMap<Pubkey, CachedPrice>>> = OnceLock::new();

fn get_price_cache() -> &'static RwLock<HashMap<Pubkey, CachedPrice>> {
    PRICE_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

// Re-export for convenience (only export what's needed)
// Note: Individual functions can be accessed via oracle::pyth::* or oracle::switchboard::*

/// Oracle price result with source information
#[derive(Debug, Clone)]
pub struct OraclePrice {
    pub price: f64,
    pub source: OracleSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleSource {
    Pyth,
    Switchboard,
}

// Re-export validation functions for convenience
// Oracle validation functions for Kamino Lend

/// Get price from reserve - HERMES FIRST, then on-chain oracles
/// Returns (price, has_pyth, has_switchboard)
/// 
/// Priority order:
/// 1. Hermes API (most reliable, always fresh)
/// 2. On-chain Pyth (may have 134 bytes legacy format issues)
/// 3. On-chain Switchboard
pub async fn get_reserve_price(
    rpc: &Arc<RpcClient>,
    reserve: &crate::kamino::Reserve,
    current_slot: u64,
) -> Result<(Option<f64>, bool, bool)> {
    let token_mint = reserve.mint_pubkey();
    let pyth_pubkey = reserve.pyth_oracle();
    let switchboard_pubkey = reserve.switchboard_oracle();
    
    // Check cache first (1 second TTL)
    let cache_key = token_mint; // Use mint as cache key for consistency
    {
        let cache = get_price_cache().read().unwrap();
        if let Some(cached) = cache.get(&cache_key) {
            if cached.timestamp.elapsed() < Duration::from_secs(1) {
                log::debug!("✅ Using cached price for {}: ${:.2} (age: {:?})", cache_key, cached.price, cached.timestamp.elapsed());
                return Ok((Some(cached.price), cached.has_pyth, cached.has_switchboard));
            }
        }
    }
    
    // ==========================================================================
    // PRIORITY 1: Hermes API (most reliable, always fresh)
    // ==========================================================================
    // 🔴 IMPORTANT: Return has_pyth=false, has_switchboard=false for Hermes
    // This signals to validation.rs to SKIP strict on-chain confidence checks
    // Hermes is Pyth's official HTTP API and already provides reliable prices
    // No additional validation needed (unlike on-chain oracles which can be stale)
    if token_mint != Pubkey::default() {
        if let Some(symbol) = get_token_symbol_from_mint(&token_mint) {
            match hermes::get_price_from_hermes(symbol).await {
                Ok(Some(hermes_price)) => {
                    log::debug!(
                        "✅ Price from Hermes API for {}: ${:.4}",
                        symbol,
                        hermes_price
                    );
                    
                    // Cache the result
                    {
                        let mut cache = get_price_cache().write().unwrap();
                        cache.insert(token_mint, CachedPrice {
                            price: hermes_price,
                            timestamp: Instant::now(),
                            has_pyth: false,       // NOT on-chain Pyth (skip strict validation)
                            has_switchboard: false, // Hermes is HTTP, no Switchboard
                        });
                    }
                    
                    // Return false, false to skip strict validation
                    // Hermes prices are already validated by Pyth network
                    return Ok((Some(hermes_price), false, false));
                }
                Ok(None) => {
                    log::debug!("⚠️  Hermes returned no price for {}, trying on-chain", symbol);
                }
                Err(e) => {
                    log::debug!("⚠️  Hermes error for {}: {}, trying on-chain", symbol, e);
                }
            }
        }
    }
    
    // ==========================================================================
    // PRIORITY 2: On-chain Pyth (fallback)
    // ==========================================================================
    if pyth_pubkey != Pubkey::default() {
        match pyth::validate_pyth_oracle(rpc, pyth_pubkey, current_slot).await {
            Ok((valid, price)) => {
                if valid {
                    if let Some(price) = price {
                        log::debug!("✅ On-chain Pyth price for {}: ${:.2}", pyth_pubkey, price);
                        
                        // Cache the result
                        {
                            let mut cache = get_price_cache().write().unwrap();
                            cache.insert(token_mint, CachedPrice {
                                price,
                                timestamp: Instant::now(),
                                has_pyth: true,
                                has_switchboard: false,
                            });
                        }
                        
                        return Ok((Some(price), true, false));
                    }
                }
            }
            Err(e) => {
                log::debug!("On-chain Pyth error: {}", e);
            }
        }
    }
    
    // ==========================================================================
    // PRIORITY 3: On-chain Switchboard (fallback)
    // ==========================================================================
    if switchboard_pubkey != Pubkey::default() {
        match switchboard::validate_switchboard_oracle_if_available(rpc, reserve, current_slot).await {
            Ok(Some(switchboard_price)) => {
                log::debug!("✅ On-chain Switchboard price: ${:.2}", switchboard_price);
                
                // Cache the result
                {
                    let mut cache = get_price_cache().write().unwrap();
                    cache.insert(token_mint, CachedPrice {
                        price: switchboard_price,
                        timestamp: Instant::now(),
                        has_pyth: false,
                        has_switchboard: true,
                    });
                }
                
                return Ok((Some(switchboard_price), false, true));
            }
            Ok(None) => {
                log::debug!("Switchboard oracle not available");
            }
            Err(e) => {
                log::debug!("Switchboard error: {}", e);
            }
        }
    }
    
    // All methods failed
    log::warn!(
        "❌ All oracle methods failed for mint {} (pyth={}, switchboard={})",
        token_mint,
        pyth_pubkey,
        switchboard_pubkey
    );
    Ok((None, false, false))
}

/// Get token symbol from known mint addresses
/// Used for Hermes API fallback when on-chain oracles fail
fn get_token_symbol_from_mint(mint: &Pubkey) -> Option<&'static str> {
    use std::str::FromStr;
    
    // Known mainnet token mints
    let sol_mint = Pubkey::from_str("So11111111111111111111111111111111111111112").ok()?;
    let usdc_mint = Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").ok()?;
    let usdt_mint = Pubkey::from_str("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB").ok()?;
    let btc_mint = Pubkey::from_str("9n4nbM75f5Ui33ZbPYXn59EwSgE8CGsHtAeTH5YFeJ9E").ok()?;
    let eth_mint = Pubkey::from_str("7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs").ok()?;
    let msol_mint = Pubkey::from_str("mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So").ok()?;
    let stsol_mint = Pubkey::from_str("7dHbWXmci3dT8UFYWYZweBLXgycu7Y3iL6trKn1Y7ARj").ok()?;
    let ray_mint = Pubkey::from_str("4k3Dyjzvzp8eMZWUXbBCjEvwSkkk59S5iCNLY3QrkX6R").ok()?;
    let srm_mint = Pubkey::from_str("SRMuApVNdxXokk5GT7XD5cUUgXMBCoAz2LHeuAoKWRt").ok()?;
    
    if *mint == sol_mint { return Some("SOL"); }
    if *mint == usdc_mint { return Some("USDC"); }
    if *mint == usdt_mint { return Some("USDT"); }
    if *mint == btc_mint { return Some("BTC"); }
    if *mint == eth_mint { return Some("ETH"); }
    if *mint == msol_mint { return Some("MSOL"); }
    if *mint == stsol_mint { return Some("STSOL"); }
    if *mint == ray_mint { return Some("RAY"); }
    if *mint == srm_mint { return Some("SRM"); }
    
    // Unknown mint - can't map to symbol
    None
}

/// Oracle configuration from environment
pub fn get_oracle_config() -> (u64, f64, u64, usize, usize, f64, f64, f64) {
    use std::env;
    
    // MAX_ORACLE_AGE_SECONDS: Maximum age of oracle price in seconds
    let max_oracle_age_secs = env::var("MAX_ORACLE_AGE_SECONDS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(300); // Default 300 seconds (5 minutes)
    
    // Convert to slots (assuming ~400ms per slot)
    let max_slot_difference = (max_oracle_age_secs * 1000 / 400).max(25); // Minimum 25 slots
    
    // MAX_ORACLE_DEVIATION_PCT: Maximum price deviation between Pyth and Switchboard
    let max_oracle_deviation_pct = env::var("MAX_ORACLE_DEVIATION_PCT")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(2.0); // Default 2%
    
    // TWAP configuration
    let twap_max_age_secs = max_oracle_age_secs.min(30); // Use oracle age or 30s, whichever is smaller
    let twap_min_samples = 5; // Minimum samples for TWAP
    let twap_max_samples = 50; // Maximum samples for TWAP
    let twap_anomaly_threshold_pct = 3.0; // 3% deviation from TWAP triggers anomaly

    // MAX_CONFIDENCE_PCT: Maximum confidence interval (percentage)
    let max_confidence_pct = env::var("MAX_CONFIDENCE_PCT")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(15.0); // Default 15%

    // MAX_CONFIDENCE_PCT_PYTH_ONLY: Stricter confidence for Pyth-only mode
    let max_confidence_pct_pyth_only = env::var("MAX_CONFIDENCE_PCT_PYTH_ONLY")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(2.0); // Default 2%
    
    (max_slot_difference, max_oracle_deviation_pct, twap_max_age_secs, twap_min_samples, twap_max_samples, twap_anomaly_threshold_pct, max_confidence_pct, max_confidence_pct_pyth_only)
}

/// Minimum valid price threshold (in USD)
const MIN_VALID_PRICE_USD: f64 = 0.01; // $0.01 minimum

pub fn min_valid_price_usd() -> f64 {
    MIN_VALID_PRICE_USD
}

