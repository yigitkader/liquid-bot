// Oracle validation module - consolidates all oracle validation logic
// Moved from pipeline.rs to reduce code size and improve maintainability

use anyhow::Result;
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::solend::Reserve;
use super::{get_oracle_config, get_reserve_price};
use super::pyth::validate_pyth_confidence_strict;
use super::switchboard::validate_switchboard_oracle_if_available;

/// Oracle price cache for TWAP calculation
/// Protects against oracle manipulation when only Pyth is available
struct OraclePriceCache {
    prices: VecDeque<(Instant, f64)>, // (timestamp, price)
    max_age: Duration,
    min_samples: usize,
}

impl OraclePriceCache {
    fn new(max_age_secs: u64, min_samples: usize) -> Self {
        OraclePriceCache {
            prices: VecDeque::new(),
            max_age: Duration::from_secs(max_age_secs),
            min_samples,
        }
    }
    
    fn add_price(&mut self, price: f64) {
        let now = Instant::now();
        
        // Time-based cleanup (remove old prices)
        self.prices.retain(|(timestamp, _)| {
            now.duration_since(*timestamp) <= self.max_age
        });
        
        self.prices.push_back((now, price));
        
        // Size-based limit (memory control)
        let (_, _, _, _, twap_max_samples, _, _, _) = get_oracle_config();
        while self.prices.len() > twap_max_samples {
            self.prices.pop_front();
        }
    }
    
    /// Calculate TWAP (Time-Weighted Average Price)
    /// Returns None if not enough samples
    fn calculate_twap(&self) -> Option<f64> {
        if self.prices.len() < self.min_samples {
            return None;
        }
        
        let now = Instant::now();
        let mut weighted_sum = 0.0;
        let mut total_weight = 0.0;
        
        for (timestamp, price) in &self.prices {
            // Weight = time since sample (older = more weight)
            let age = now.duration_since(*timestamp).as_secs_f64();
            let weight = 1.0 / (1.0 + age); // Exponential decay
            
            weighted_sum += price * weight;
            total_weight += weight;
        }
        
        if total_weight > 0.0 {
            Some(weighted_sum / total_weight)
        } else {
            None
        }
    }
    
    /// Check if current price deviates too much from TWAP
    /// Returns true if deviation > threshold
    fn is_price_anomaly(&self, current_price: f64, threshold_pct: f64) -> bool {
        if let Some(twap) = self.calculate_twap() {
            let deviation_pct = ((current_price - twap).abs() / twap) * 100.0;
            deviation_pct > threshold_pct
        } else {
            false // Not enough data, assume OK
        }
    }
}

/// Global price cache for TWAP calculation (per reserve)
/// Uses OnceLock<RwLock<HashMap>> for thread-safe access
static PRICE_CACHES: OnceLock<RwLock<HashMap<Pubkey, OraclePriceCache>>> = 
    OnceLock::new();

/// Get or initialize the global price cache
fn get_price_caches() -> &'static RwLock<HashMap<Pubkey, OraclePriceCache>> {
    PRICE_CACHES.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Returns (is_valid, borrow_price_usd, deposit_price_usd)
pub async fn validate_oracles(
    rpc: &Arc<RpcClient>,
    borrow_reserve: &Option<Reserve>,
    deposit_reserve: &Option<Reserve>,
) -> Result<(bool, Option<f64>, Option<f64>)> {
    log::debug!("🔍 Starting oracle validation...");
    
    // Check if reserves exist and have EITHER Pyth OR Switchboard oracle
    let borrow_ok = borrow_reserve
        .as_ref()
        .map(|r| {
            let pyth_oracle = r.oracle_pubkey();
            let switchboard_oracle = r.liquidity().liquiditySwitchboardOracle;
            let has_pyth = pyth_oracle != Pubkey::default();
            let has_switchboard = switchboard_oracle != Pubkey::default();
            
            log::debug!("  Borrow reserve oracle check: Pyth={} ({:?}), Switchboard={} ({:?})", 
                       has_pyth, if has_pyth { Some(pyth_oracle) } else { None },
                       has_switchboard, if has_switchboard { Some(switchboard_oracle) } else { None });
            
            has_pyth || has_switchboard
        })
        .unwrap_or(false);

    let deposit_ok = deposit_reserve
        .as_ref()
        .map(|r| {
            let pyth_oracle = r.oracle_pubkey();
            let switchboard_oracle = r.liquidity().liquiditySwitchboardOracle;
            let has_pyth = pyth_oracle != Pubkey::default();
            let has_switchboard = switchboard_oracle != Pubkey::default();
            
            log::debug!("  Deposit reserve oracle check: Pyth={} ({:?}), Switchboard={} ({:?})", 
                       has_pyth, if has_pyth { Some(pyth_oracle) } else { None },
                       has_switchboard, if has_switchboard { Some(switchboard_oracle) } else { None });
            
            has_pyth || has_switchboard
        })
        .unwrap_or(false);

    // ✅ CRITICAL FIX: Fallback oracle validation when reserve parsing fails
    // If reserves are None (parsing failed) OR missing oracle pubkeys, try to validate oracles from env variables
    // This handles the case where Reserve::from_account_data() fails but oracle still works
    if borrow_reserve.is_none() || deposit_reserve.is_none() || !borrow_ok || !deposit_ok {
        log::warn!("⚠️  Reserve parsing failed or missing oracle pubkeys, attempting fallback oracle validation from env");
        log::warn!("   Borrow reserve: {:?}, Deposit reserve: {:?}", 
                  if borrow_reserve.is_some() { "parsed" } else { "None (parsing failed)" },
                  if deposit_reserve.is_some() { "parsed" } else { "None (parsing failed)" });
        log::warn!("   Borrow reserve has oracle: {}, Deposit reserve has oracle: {}", borrow_ok, deposit_ok);
        
        // Try to get oracle pubkeys from environment variables
        use std::env;
        use std::str::FromStr;
        use super::pyth::validate_pyth_oracle;
        
        if let Ok(pyth_feed_str) = env::var("SOL_USD_PYTH_FEED") {
            if let Ok(pyth_pubkey) = Pubkey::from_str(&pyth_feed_str) {
                log::debug!("  🔄 Attempting fallback validation with Pyth feed from env: {}", pyth_pubkey);
                
                let current_slot = rpc
                    .get_slot()
                    .map_err(|e| anyhow::anyhow!("Failed to get current slot: {}", e))?;
                
                match validate_pyth_oracle(rpc, pyth_pubkey, current_slot).await {
                    Ok((true, Some(price))) => {
                        log::info!("✅ Fallback oracle validation successful: Pyth price=${:.2}", price);
                        log::info!("   Using fallback price for both borrow and deposit reserves");
                        // Return same price for both borrow and deposit (fallback mode)
                        return Ok((true, Some(price), Some(price)));
                    }
                    Ok((true, None)) => {
                        // Oracle is valid but price is None (shouldn't happen, but handle it)
                        log::warn!("⚠️  Fallback oracle validation: Oracle is valid but price is None");
                    }
                    Ok((false, _)) => {
                        log::warn!("❌ Fallback oracle validation failed: Pyth oracle invalid");
                    }
                    Err(e) => {
                        log::warn!("❌ Fallback oracle validation error: {}", e);
                    }
                }
            } else {
                log::warn!("❌ Invalid SOL_USD_PYTH_FEED pubkey format: {}", pyth_feed_str);
            }
        } else {
            log::warn!("❌ SOL_USD_PYTH_FEED not found in environment variables");
        }
        
        // If fallback also failed, return error
        log::error!("❌ Oracle validation failed: missing oracle pubkeys (neither Pyth nor Switchboard)");
        log::error!("   Borrow reserve has oracle: {}", borrow_ok);
        log::error!("   Deposit reserve has oracle: {}", deposit_ok);
        log::error!("   Fallback validation from env also failed");
        return Ok((false, None, None));
    }
    
    log::debug!("✅ Both reserves have oracle pubkeys, proceeding with price validation...");

    // Get current slot for stale check
    let current_slot = rpc
        .get_slot()
        .map_err(|e| anyhow::anyhow!("Failed to get current slot: {}", e))?;

    // Validate borrow reserve oracle - try Pyth first, fallback to Switchboard
    log::debug!("  📡 Validating borrow reserve oracle...");
    let (borrow_price, borrow_has_pyth, borrow_has_switchboard_for_crossval) = if let Some(reserve) = borrow_reserve {
        let oracle_start = std::time::Instant::now();
        match get_reserve_price(rpc, reserve, current_slot).await {
            Ok((price, has_pyth, has_switchboard)) => {
                let oracle_duration = oracle_start.elapsed();
                if price.is_none() {
                    log::warn!("❌ Borrow reserve: No valid oracle price found (Pyth: {}, Switchboard: {}) after {:?}", 
                              has_pyth, has_switchboard, oracle_duration);
                    return Ok((false, None, None));
                }
                log::debug!("  ✅ Borrow reserve oracle validated in {:?}: price=${:.6}, Pyth={}, Switchboard={}", 
                           oracle_duration, price.unwrap(), has_pyth, has_switchboard);
                (price, has_pyth, has_switchboard)
            }
            Err(e) => {
                log::warn!("❌ Borrow reserve oracle error: {}", e);
                return Ok((false, None, None));
            }
        }
    } else {
        log::debug!("  ⏭️  No borrow reserve to validate");
        (None, false, false)
    };

    // Validate deposit reserve oracle - try Pyth first, fallback to Switchboard
    log::debug!("  📡 Validating deposit reserve oracle...");
    let (deposit_price, deposit_has_pyth, deposit_has_switchboard_for_crossval) = if let Some(reserve) = deposit_reserve {
        let oracle_start = std::time::Instant::now();
        match get_reserve_price(rpc, reserve, current_slot).await {
            Ok((price, has_pyth, has_switchboard)) => {
                let oracle_duration = oracle_start.elapsed();
                if price.is_none() {
                    log::warn!("❌ Deposit reserve: No valid oracle price found (Pyth: {}, Switchboard: {}) after {:?}", 
                              has_pyth, has_switchboard, oracle_duration);
                    return Ok((false, None, None));
                }
                log::debug!("  ✅ Deposit reserve oracle validated in {:?}: price=${:.6}, Pyth={}, Switchboard={}", 
                           oracle_duration, price.unwrap(), has_pyth, has_switchboard);
                (price, has_pyth, has_switchboard)
            }
            Err(e) => {
                log::warn!("❌ Deposit reserve oracle error: {}", e);
                return Ok((false, None, None));
            }
        }
    } else {
        log::debug!("  ⏭️  No deposit reserve to validate");
        (None, false, false)
    };

    // Cross-validate: If we have both Pyth and Switchboard, compare them
    let (_, max_oracle_deviation_pct, _, _, _, _, max_confidence_pct, max_confidence_pct_pyth_only) = get_oracle_config();
    
    if borrow_has_pyth && borrow_has_switchboard_for_crossval {
        if let (Some(borrow_reserve), Some(borrow_price)) = (borrow_reserve, borrow_price) {
            if let Some(switchboard_price) = validate_switchboard_oracle_if_available(
                rpc,
                borrow_reserve,
                current_slot,
            )
            .await?
            {
                // Compare Pyth and Switchboard prices
                let deviation_pct = ((borrow_price - switchboard_price).abs() / borrow_price) * 100.0;
                if deviation_pct > max_oracle_deviation_pct {
                    log::warn!(
                        "Oracle deviation too high for borrow reserve: {:.2}% > {:.2}% (Pyth: {}, Switchboard: {})",
                        deviation_pct,
                        max_oracle_deviation_pct,
                        borrow_price,
                        switchboard_price
                    );
                    return Ok((false, None, None));
                }
                log::debug!(
                    "✅ Oracle deviation OK for borrow reserve: {:.2}% (Pyth: {}, Switchboard: {})",
                    deviation_pct,
                    borrow_price,
                    switchboard_price
                );
            }
        }
    }

    if deposit_has_pyth && deposit_has_switchboard_for_crossval {
        if let (Some(deposit_reserve), Some(deposit_price)) = (deposit_reserve, deposit_price) {
            if let Some(switchboard_price) = validate_switchboard_oracle_if_available(
                rpc,
                deposit_reserve,
                current_slot,
            )
            .await?
            {
                let deviation_pct = ((deposit_price - switchboard_price).abs() / deposit_price) * 100.0;
                if deviation_pct > max_oracle_deviation_pct {
                    log::warn!(
                        "Oracle deviation too high for deposit reserve: {:.2}% > {:.2}% (Pyth: {}, Switchboard: {})",
                        deviation_pct,
                        max_oracle_deviation_pct,
                        deposit_price,
                        switchboard_price
                    );
                    return Ok((false, None, None));
                }
                log::debug!(
                    "✅ Oracle deviation OK for deposit reserve: {:.2}% (Pyth: {}, Switchboard: {})",
                    deviation_pct,
                    deposit_price,
                    switchboard_price
                );
            }
        }
    }

    // SECURITY: If Switchboard is NOT available AND we're using Pyth, apply stricter Pyth validation
    let borrow_needs_stricter_validation = borrow_has_pyth && !borrow_has_switchboard_for_crossval;
    let deposit_needs_stricter_validation = deposit_has_pyth && !deposit_has_switchboard_for_crossval;
    
    if borrow_needs_stricter_validation || deposit_needs_stricter_validation {
        log::warn!(
            "⚠️  SECURITY WARNING: Switchboard oracle not available for cross-validation (borrow: {}, deposit: {}). \
             Using Pyth-only with stricter confidence threshold ({}% vs {}%). \
             This increases risk of oracle manipulation.",
            if borrow_has_switchboard_for_crossval { "✓" } else { "✗" },
            if deposit_has_switchboard_for_crossval { "✓" } else { "✗" },
            max_confidence_pct_pyth_only,
            max_confidence_pct
        );
        
        // Re-validate Pyth confidence with stricter threshold
        if borrow_needs_stricter_validation {
            if let (Some(borrow_reserve), Some(borrow_price)) = (borrow_reserve, borrow_price) {
                let confidence_check = validate_pyth_confidence_strict(
                    rpc,
                    borrow_reserve.oracle_pubkey(),
                    borrow_price,
                    current_slot,
                ).await?;
                if !confidence_check {
                    log::warn!(
                        "Borrow reserve Pyth confidence check failed (stricter threshold for Pyth-only mode)"
                    );
                    return Ok((false, None, None));
                }
            }
        }
        
        if deposit_needs_stricter_validation {
            if let (Some(deposit_reserve), Some(deposit_price)) = (deposit_reserve, deposit_price) {
                let confidence_check = validate_pyth_confidence_strict(
                    rpc,
                    deposit_reserve.oracle_pubkey(),
                    deposit_price,
                    current_slot,
                ).await?;
                if !confidence_check {
                    log::warn!(
                        "Deposit reserve Pyth confidence check failed (stricter threshold for Pyth-only mode)"
                    );
                    return Ok((false, None, None));
                }
            }
        }
    }

    Ok((true, borrow_price, deposit_price))
}

/// Enhanced oracle validation with TWAP protection
/// This function adds TWAP (Time-Weighted Average Price) protection when Switchboard is not available
/// to detect oracle manipulation attempts
/// 
/// Returns (is_valid, borrow_price_usd, deposit_price_usd)
pub async fn validate_oracles_with_twap(
    rpc: &Arc<RpcClient>,
    borrow_reserve: &Option<Reserve>,
    deposit_reserve: &Option<Reserve>,
) -> Result<(bool, Option<f64>, Option<f64>)> {
    log::debug!("🔍 Starting enhanced oracle validation with TWAP protection...");
    
    // First, get current prices from standard oracle validation
    let (pyth_ok, borrow_price, deposit_price) = 
        validate_oracles(rpc, borrow_reserve, deposit_reserve).await?;
    
    if !pyth_ok {
        log::warn!("❌ Standard oracle validation failed, skipping TWAP check");
        return Ok((false, None, None));
    }
    
    log::debug!("✅ Standard oracle validation passed, proceeding with TWAP protection check...");

    // Check if TWAP protection is enabled via environment variable
    // CRITICAL: Default is ENABLED (true) to protect against oracle manipulation in Pyth-only mode
    // TWAP protection is essential when Switchboard is not available for cross-validation
    // Only disable if explicitly set to false via ENABLE_TWAP_PROTECTION=false
    let enable_twap = std::env::var("ENABLE_TWAP_PROTECTION")
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(true); // ✅ DEFAULT: ENABLED (protects against oracle manipulation)

    if !enable_twap {
        log::warn!("⚠️  TWAP protection DISABLED (ENABLE_TWAP_PROTECTION=false) - Oracle manipulation risk increased!");
        return Ok((true, borrow_price, deposit_price));
    }
    
    // Check if Switchboard is available for either reserve
    // If Switchboard is available, we don't need TWAP protection (cross-validation is sufficient)
    let current_slot = rpc
        .get_slot()
        .map_err(|e| anyhow::anyhow!("Failed to get current slot: {}", e))?;
    
    let mut borrow_has_switchboard = false;
    let mut deposit_has_switchboard = false;
    
    if let Some(reserve) = borrow_reserve {
        if let Ok(Some(_)) = validate_switchboard_oracle_if_available(rpc, reserve, current_slot).await {
            borrow_has_switchboard = true;
        }
    }
    
    if let Some(reserve) = deposit_reserve {
        if let Ok(Some(_)) = validate_switchboard_oracle_if_available(rpc, reserve, current_slot).await {
            deposit_has_switchboard = true;
        }
    }
    
    // Only apply TWAP protection if Switchboard is NOT available (Pyth-only mode)
    if !borrow_has_switchboard || !deposit_has_switchboard {
        let caches = get_price_caches();
        let mut caches_guard = match caches.write() {
            Ok(guard) => guard,
            Err(e) => {
                log::error!("Failed to acquire write lock on price caches: {}", e);
                // Fail safe - if we can't check oracle safety, assume unsafe
                return Ok((false, None, None));
            }
        };
        
        // Get or create price cache for borrow reserve
        let (_, _, twap_max_age_secs, twap_min_samples, _, twap_anomaly_threshold_pct, _, _) = get_oracle_config();
        if let (Some(reserve), Some(price)) = (borrow_reserve.as_ref(), borrow_price) {
            if !borrow_has_switchboard {
                let oracle_pubkey = reserve.oracle_pubkey();
                let borrow_cache = caches_guard
                    .entry(oracle_pubkey)
                    .or_insert_with(|| OraclePriceCache::new(twap_max_age_secs, twap_min_samples));
                
                // Add current price to cache
                borrow_cache.add_price(price);
                
                // Check for manipulation
                if borrow_cache.is_price_anomaly(price, twap_anomaly_threshold_pct) {
                    if let Some(twap) = borrow_cache.calculate_twap() {
                        log::warn!(
                            "⚠️  Borrow price anomaly detected! Current: ${:.6}, TWAP: ${:.6}, Deviation: {:.2}%",
                            price,
                            twap,
                            ((price - twap).abs() / twap) * 100.0
                        );
                    } else {
                        log::warn!(
                            "⚠️  Borrow price anomaly detected! Current: ${:.6} (TWAP not available yet)",
                            price
                        );
                    }
                    return Ok((false, None, None));
                }
            }
        }
        
        // Get or create price cache for deposit reserve
        if let (Some(reserve), Some(price)) = (deposit_reserve.as_ref(), deposit_price) {
            if !deposit_has_switchboard {
                let oracle_pubkey = reserve.oracle_pubkey();
                let deposit_cache = caches_guard
                    .entry(oracle_pubkey)
                    .or_insert_with(|| OraclePriceCache::new(twap_max_age_secs, twap_min_samples));
                
                // Add current price to cache
                deposit_cache.add_price(price);
                
                // Check for manipulation
                if deposit_cache.is_price_anomaly(price, twap_anomaly_threshold_pct) {
                    if let Some(twap) = deposit_cache.calculate_twap() {
                        log::warn!(
                            "⚠️  Deposit price anomaly detected! Current: ${:.6}, TWAP: ${:.6}, Deviation: {:.2}%",
                            price,
                            twap,
                            ((price - twap).abs() / twap) * 100.0
                        );
                    } else {
                        log::warn!(
                            "⚠️  Deposit price anomaly detected! Current: ${:.6} (TWAP not available yet)",
                            price
                        );
                    }
                    return Ok((false, None, None));
                }
            }
        }
    }
    
    log::debug!("✅ Enhanced oracle validation with TWAP protection completed successfully");
    log::debug!("   Final prices: Borrow=${:?}, Deposit=${:?}", borrow_price, deposit_price);
    
    Ok((true, borrow_price, deposit_price))
}

