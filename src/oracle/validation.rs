// Oracle validation module - consolidates all oracle validation logic
// Moved from pipeline.rs to reduce code size and improve maintainability

use anyhow::Result;
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

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

// Oracle validation functions for Kamino Lend

