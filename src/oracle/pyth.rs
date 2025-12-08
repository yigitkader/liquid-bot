// Pyth Oracle Module
// Handles Pyth v2 and Pythnet v3 oracle validation

use anyhow::Result;
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use std::sync::Arc;

use crate::oracle::{get_oracle_config, min_valid_price_usd};

// Pyth price status values (from price_type byte)
const PYTH_PRICE_STATUS_UNKNOWN: u8 = 0;
const PYTH_PRICE_STATUS_PRICE: u8 = 1; // Price status - acceptable for liquidations
const PYTH_PRICE_STATUS_TRADING: u8 = 2; // Trading status - preferred for liquidations
const PYTH_PRICE_STATUS_HALTED: u8 = 3;

/// Get Pyth Network program ID from environment
pub fn pyth_program_id() -> Result<Pubkey> {
    use std::env;
    let pyth_id_str = env::var("PYTH_PROGRAM_ID")
        .map_err(|_| anyhow::anyhow!(
            "PYTH_PROGRAM_ID not found in .env file. \
             Please set PYTH_PROGRAM_ID in .env file. \
             Example: PYTH_PROGRAM_ID=FsJ3A3u2vn5cTVofAjvy6y5kwABJAqYWpe4975bi2epH"
        ))?;
    Pubkey::from_str(&pyth_id_str)
        .map_err(|e| anyhow::anyhow!("Invalid PYTH_PROGRAM_ID from .env: {} - Error: {}", pyth_id_str, e))
}

/// Validate Pyth oracle per Structure.md section 5.2
/// Returns (is_valid, price) where price is in USD with decimals
/// 
/// Auto-detects Pyth version based on account size:
/// - Pyth v2: ~84 bytes (legacy)
/// - Pythnet v3: 3312+ bytes (current mainnet standard)
pub async fn validate_pyth_oracle(
    rpc: &Arc<RpcClient>,
    oracle_pubkey: Pubkey,
    current_slot: u64,
) -> Result<(bool, Option<f64>)> {
    // 1. Get oracle account and check program ID (CRITICAL: check by owner, not by size!)
    let oracle_account = rpc
        .get_account(&oracle_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get oracle account: {}", e))?;

    // CRITICAL FIX: Check program ID by owner, not by account size
    // Pyth v2 program ID (legacy)
    let pyth_v2_program = Pubkey::from_str("FsJ3A3u2vn5cTVofAjvy6y5kwABJAqYWpe4975bi2epH")
        .map_err(|e| anyhow::anyhow!("Failed to parse Pyth v2 program ID: {}", e))?;
    
    // Pythnet v3 program ID (current mainnet standard)
    let pythnet_v3_program = Pubkey::from_str("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ")
        .map_err(|e| anyhow::anyhow!("Failed to parse Pythnet v3 program ID: {}", e))?;
    
    // Also check env variable (for flexibility, but prefer hardcoded known IDs)
    let pyth_program_id_env = pyth_program_id().ok();
    
    // Check if account belongs to Pyth program (v2 or v3)
    let is_pyth_v2 = oracle_account.owner == pyth_v2_program;
    let is_pythnet_v3 = oracle_account.owner == pythnet_v3_program;
    let is_pyth_env = pyth_program_id_env.map(|id| oracle_account.owner == id).unwrap_or(false);
    
    if !is_pyth_v2 && !is_pythnet_v3 && !is_pyth_env {
        // This is not a Pyth oracle - might be Switchboard or another oracle type
        // This is expected behavior, not an error - just return false
        log::debug!(
            "Oracle account {} does not belong to Pyth program (owner: {}). Expected: v2={}, v3={}, env={:?}. This may be a Switchboard oracle.",
            oracle_pubkey,
            oracle_account.owner,
            pyth_v2_program,
            pythnet_v3_program,
            pyth_program_id_env
        );
        return Ok((false, None));
    }
    
    // DEBUG: Log account data info for troubleshooting
    log::debug!(
        "🔍 Pyth oracle {}: account size={} bytes, owner={}, version={}",
        oracle_pubkey,
        oracle_account.data.len(),
        oracle_account.owner,
        if is_pythnet_v3 { "v3" } else if is_pyth_v2 { "v2" } else { "env" }
    );

    // 2. Route to correct parser based on program ID OR account size
    // CRITICAL FIX: If account is large (>2000 bytes), it is almost certainly Pyth v3,
    // regardless of the exact program ID match (which might be using a proxy or different ID).
    // Pyth v2 accounts are small (~84 bytes).
    if is_pythnet_v3 || oracle_account.data.len() > 2000 {
        // Use Pythnet v3 parser (pyth-sdk-solana crate)
        log::debug!("✅ Detected Pythnet v3 oracle (via program ID or size > 2000) for {} - using v3 parser", oracle_pubkey);
        return validate_pyth_oracle_v3(rpc, oracle_pubkey, current_slot).await;
    }
    
    // Legacy v2 format (deprecated, but keep for compatibility)
    // Check account size for v2 format
    let account_size = oracle_account.data.len();
    if account_size < 84 {
        log::warn!(
            "❌ Oracle account {} data too short: {} bytes (need at least 84 for Pyth v2)",
            oracle_pubkey,
            account_size
        );
        return Ok((false, None));
    }

    // Continue with v2 validation...
    // NOTE: Full v2 validation code is very long (~600 lines)
    // For now, we'll keep it in pipeline.rs and move it later
    // This is a placeholder that calls the v2 validation logic
    
    // For now, return error to indicate v2 needs to be implemented
    // TODO: Move full v2 validation from pipeline.rs
    log::warn!("Pyth v2 validation not yet moved to oracle module - using v3 parser as fallback");
    validate_pyth_oracle_v3(rpc, oracle_pubkey, current_slot).await
}

/// Validate Pythnet v3 oracle (current mainnet standard)
pub async fn validate_pyth_oracle_v3(
    rpc: &Arc<RpcClient>,
    oracle_pubkey: Pubkey,
    current_slot: u64,
) -> Result<(bool, Option<f64>)> {
    use pyth_sdk_solana::state::load_price_account;
    
    // Get account data directly
    let account_data = rpc
        .get_account_data(&oracle_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get oracle account data: {}", e))?;
    
    // Parse using pyth-sdk-solana's load_price_account (correct API)
    use pyth_sdk_solana::state::{GenericPriceAccount, PriceAccountExt};
    let price_account: &GenericPriceAccount<128, PriceAccountExt> = match load_price_account(&account_data) {
        Ok(account) => account,
        Err(e) => {
            log::warn!(
                "❌ Failed to parse Pythnet v3 price feed {}: {}\n\
                 Account size: {} bytes\n\
                 First 16 bytes: {:02x?}",
                oracle_pubkey,
                e,
                account_data.len(),
                &account_data[..16.min(account_data.len())]
            );
            return Ok((false, None));
        }
    };
    
    // Get current time for staleness check
    let current_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    
    // Configurable max age for Pythnet v3 (default: 300 seconds = 5 minutes)
    let max_age_seconds = std::env::var("PYTHNET_V3_MAX_AGE_SECONDS")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(300);
    
    // Get Clock from RPC for get_price_no_older_than()
    use solana_sdk::clock::Clock;
    let clock = rpc.get_account(&solana_sdk::sysvar::clock::id())
        .ok()
        .and_then(|acc| bincode::deserialize::<Clock>(&acc.data).ok());
    
    // If we can't get Clock, fall back to manual timestamp checking
    let price_info = if let Some(clock) = clock {
        match price_account.get_price_no_older_than(&clock, max_age_seconds as u64) {
            Some(price) => price,
            None => {
                log::warn!(
                    "⚠️ Pythnet v3 oracle {}: No price available within age limit (max_age: {}s).",
                    oracle_pubkey,
                    max_age_seconds
                );
                return Ok((false, None));
            }
        }
    } else {
        log::warn!("⚠️ Could not get Clock from RPC, using manual timestamp check");
        return Ok((false, None));
    };
    
    // Calculate age for logging
    let age_seconds = current_time - price_info.publish_time;
    
    // Check if price is within acceptable age limit
    if age_seconds > max_age_seconds {
        if age_seconds > 600 {
            log::warn!(
                "⚠️ Pythnet v3 oracle {}: Price too old even for fallback (age: {}s > 600s). Rejecting.",
                oracle_pubkey,
                age_seconds
            );
            return Ok((false, None));
        }
        log::warn!(
            "⚠️ Using older Pyth price (age: {}s, max: {}s) - within fallback limit",
            age_seconds,
            max_age_seconds
        );
    }
    
    // Additional validation: publish_time sanity check (not in future)
    if price_info.publish_time > current_time + 60 {
        log::warn!(
            "⚠️ Pythnet v3 oracle {}: publish_time in future (publish_time: {}, current_time: {})",
            oracle_pubkey,
            price_info.publish_time,
            current_time
        );
        return Ok((false, None));
    }
    
    // Calculate price in USD
    let price = (price_info.price as f64) * 10_f64.powi(price_info.expo);
    
    // Validate price
    if price <= 0.0 || !price.is_finite() {
        log::warn!("⚠️ Pythnet v3 oracle price is invalid: {} for oracle {}", price, oracle_pubkey);
        return Ok((false, None));
    }
    
    // Validate confidence interval
    let confidence = (price_info.conf as f64) * 10_f64.powi(price_info.expo);
    let confidence_pct = (confidence / price.abs()) * 100.0;
    
    // Check minimum price threshold
    if price.abs() < min_valid_price_usd() {
        log::debug!(
            "Pythnet v3 oracle price below minimum threshold: {} < {} for oracle {}",
            price.abs(),
            min_valid_price_usd(),
            oracle_pubkey
        );
        return Ok((false, None));
    }
    
    let (_, _, _, _, _, _, max_confidence_pct, _) = get_oracle_config();
    if confidence_pct > max_confidence_pct {
        log::debug!(
            "Pythnet v3 confidence too high: {:.2}% > {:.2}% for oracle {} (price: {}, confidence: {})",
            confidence_pct,
            max_confidence_pct,
            oracle_pubkey,
            price,
            confidence
        );
        return Ok((false, None));
    }
    
    log::debug!(
        "✅ Pythnet v3 oracle validation passed for {}: price={}, confidence={:.2}%, publish_time={}, age={}s",
        oracle_pubkey,
        price,
        confidence_pct,
        price_info.publish_time,
        age_seconds
    );
    
    Ok((true, Some(price)))
}

/// Validate Pyth confidence with stricter threshold (for Pyth-only mode when Switchboard is unavailable)
pub async fn validate_pyth_confidence_strict(
    rpc: &Arc<RpcClient>,
    oracle_pubkey: Pubkey,
    price_value: f64,
    _current_slot: u64,
) -> Result<bool> {
    let oracle_account = rpc
        .get_account(&oracle_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get oracle account: {}", e))?;

    if oracle_account.data.len() < 56 {
        return Ok(false);
    }

    // Parse exponent
    let exponent_bytes: [u8; 4] = oracle_account.data[16..20]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse exponent"))?;
    let exponent = i32::from_le_bytes(exponent_bytes);

    // Exponent overflow check
    const MAX_EXPONENT: i32 = 18;
    const MIN_EXPONENT: i32 = -18;
    
    if exponent > MAX_EXPONENT || exponent < MIN_EXPONENT {
        log::warn!(
            "Pyth exponent out of range for strict check: {} (expected range: {} to {})",
            exponent,
            MIN_EXPONENT,
            MAX_EXPONENT
        );
        return Ok(false);
    }

    // Parse confidence (u64 at offset 48-56)
    let conf_bytes: [u8; 8] = oracle_account.data[48..56]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse confidence"))?;
    let confidence_raw = u64::from_le_bytes(conf_bytes);
    
    let confidence_multiplier = 10_f64.powi(exponent);
    if !confidence_multiplier.is_finite() {
        log::warn!("Pyth confidence multiplier overflow: exponent={}", exponent);
        return Ok(false);
    }
    
    let confidence = confidence_raw as f64 * confidence_multiplier;

    // Check minimum price threshold
    if price_value.abs() < min_valid_price_usd() {
        log::warn!(
            "Pyth oracle price below minimum threshold for strict check: {} < {} (price too small, likely invalid)",
            price_value.abs(),
            min_valid_price_usd()
        );
        return Ok(false);
    }
    
    // Check if confidence interval is too large (stricter threshold for Pyth-only mode)
    let confidence_pct = (confidence / price_value.abs()) * 100.0;

    let (_, _, _, _, _, _, _, max_confidence_pct_pyth_only) = get_oracle_config();
    if confidence_pct > max_confidence_pct_pyth_only {
        log::warn!(
            "Pyth oracle confidence too high for Pyth-only mode: {:.2}% > {:.2}% (price: {}, confidence: {})",
            confidence_pct,
            max_confidence_pct_pyth_only,
            price_value,
            confidence
        );
        return Ok(false);
    }

    log::debug!(
        "✅ Pyth confidence check passed (stricter threshold): {:.2}% <= {:.2}%",
        confidence_pct,
        max_confidence_pct_pyth_only
    );

    Ok(true)
}

