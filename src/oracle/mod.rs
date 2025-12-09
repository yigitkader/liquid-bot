// Oracle module - coordinates between Pyth and Switchboard oracles
// Separated from pipeline.rs to reduce code size and improve maintainability

pub mod pyth;
pub mod switchboard;

use anyhow::Result;
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;

use crate::solend::Reserve;

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

/// Get price from reserve, trying Pyth first, then Switchboard
/// Returns (price, has_pyth, has_switchboard)
pub async fn get_reserve_price(
    rpc: &Arc<RpcClient>,
    reserve: &Reserve,
    current_slot: u64,
) -> Result<(Option<f64>, bool, bool)> {
    let pyth_pubkey = reserve.oracle_pubkey();
    let switchboard_pubkey = reserve.liquidity().liquiditySwitchboardOracle;
    
    // Try Pyth first if available
    if pyth_pubkey != Pubkey::default() {
        match pyth::validate_pyth_oracle(rpc, pyth_pubkey, current_slot).await {
            Ok((valid, price)) => {
                if valid {
                    if let Some(price) = price {
                        // Pyth succeeded - check if Switchboard is also available for cross-validation
                        let has_switchboard = if switchboard_pubkey != Pubkey::default() {
                            switchboard::validate_switchboard_oracle_if_available(rpc, reserve, current_slot)
                                .await?
                                .is_some()
                        } else {
                            false
                        };
                        log::debug!("✅ Pyth oracle validation succeeded for reserve {}: price=${:.2}", pyth_pubkey, price);
                        return Ok((Some(price), true, has_switchboard));
                    } else {
                        log::debug!("Pyth oracle {} validation returned valid=true but price=None, trying Switchboard", pyth_pubkey);
                    }
                } else {
                    log::debug!("Pyth oracle {} validation failed (may be Switchboard oracle), trying Switchboard", pyth_pubkey);
                }
            }
            Err(e) => {
                log::debug!("Pyth oracle {} validation error: {}, trying Switchboard", pyth_pubkey, e);
            }
        }
    }
    
    // Pyth not available or failed - try Switchboard
    if switchboard_pubkey != Pubkey::default() {
        match switchboard::validate_switchboard_oracle_if_available(rpc, reserve, current_slot).await {
            Ok(Some(switchboard_price)) => {
                log::debug!("Using Switchboard price (Pyth not available or failed)");
                return Ok((Some(switchboard_price), false, true));
            }
            Ok(None) => {
                log::debug!("Switchboard oracle not available or invalid");
            }
            Err(e) => {
                log::debug!("Switchboard oracle validation error: {}", e);
            }
        }
    }
    
    // Neither oracle worked
    Ok((None, false, false))
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

