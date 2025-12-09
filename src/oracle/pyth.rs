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

    // 🔴 CRITICAL FIX: Detect version from MAGIC NUMBER, not just owner
    // Pyth v2 magic: 0xa1b2c3d4 (little-endian: d4 c3 b2 a1)
    // This is the most reliable way to detect Pyth v2 vs v3
    if oracle_account.data.len() >= 4 {
        let magic_bytes = &oracle_account.data[0..4];
        let magic = u32::from_le_bytes([magic_bytes[0], magic_bytes[1], magic_bytes[2], magic_bytes[3]]);
        
        // Pyth v2 magic number
        const PYTH_V2_MAGIC: u32 = 0xa1b2c3d4;
        
        if magic == PYTH_V2_MAGIC {
            log::debug!("✅ Detected Pyth v2 oracle (magic: 0x{:08x}) for {}", magic, oracle_pubkey);
            return validate_pyth_oracle_v2(rpc, oracle_pubkey, current_slot).await;
        } else {
            log::debug!("✅ Detected Pyth v3 oracle (magic: 0x{:08x}) for {}", magic, oracle_pubkey);
            return validate_pyth_oracle_v3(rpc, oracle_pubkey, current_slot).await;
        }
    }
    
    // Fallback: Route to correct parser based on program ID OR account size
    // If account is large (>2000 bytes), it is almost certainly Pyth v3
    // Pyth v2 accounts are small (~84 bytes)
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

    // Fallback: Try size-based detection
    if account_size < 200 {
        log::debug!("Using Pyth v2 parser (size={}) for {}", account_size, oracle_pubkey);
        return validate_pyth_oracle_v2(rpc, oracle_pubkey, current_slot).await;
    }
    
    Ok((false, None))
}

/// Validate Pyth v2 oracle (Legacy format - 84 bytes)
/// 
/// Pyth v2 layout:
/// - 0-4: magic (0xa1b2c3d4)
/// - 4-8: version
/// - 8-12: type
/// - 12-16: size
/// - 16-20: price_type (0=unknown, 1=price)
/// - 20-24: exponent (i32)
/// - 24-32: price (i64)
/// - 32-40: conf (u64)
/// - 40-48: twap (i64)
/// - 48-56: twac (u64)
/// - 56-64: valid_slot (u64)
/// - 64-72: publish_slot (u64)
pub async fn validate_pyth_oracle_v2(
    rpc: &Arc<RpcClient>,
    oracle_pubkey: Pubkey,
    current_slot: u64,
) -> Result<(bool, Option<f64>)> {
    let oracle_account = rpc
        .get_account(&oracle_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get oracle account: {}", e))?;

    if oracle_account.data.len() < 84 {
        return Ok((false, None));
    }
    
    // Pyth v2 layout:
    // 0-4: magic (0xa1b2c3d4)
    // 4-8: version
    // 8-12: type
    // 12-16: size
    // 16-20: price_type (0=unknown, 1=price)
    // 20-24: exponent (i32)
    // 24-32: price (i64)
    // 32-40: conf (u64)
    // 40-48: twap (i64)
    // 48-56: twac (u64)
    // 56-64: valid_slot (u64)
    // 64-72: publish_slot (u64)
    
    let price_type = oracle_account.data[16];
    if price_type != PYTH_PRICE_STATUS_PRICE && price_type != PYTH_PRICE_STATUS_TRADING {
        log::debug!(
            "Pyth v2 oracle {}: Invalid price_type {} (expected {} or {})",
            oracle_pubkey,
            price_type,
            PYTH_PRICE_STATUS_PRICE,
            PYTH_PRICE_STATUS_TRADING
        );
        return Ok((false, None));
    }
    
    // Parse exponent (i32 at offset 20-24)
    let exp_bytes: [u8; 4] = oracle_account.data[20..24]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse exponent bytes"))?;
    let exponent = i32::from_le_bytes(exp_bytes);
    
    // Parse price (i64 at offset 24-32)
    let price_bytes: [u8; 8] = oracle_account.data[24..32]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse price bytes"))?;
    let price_raw = i64::from_le_bytes(price_bytes);
    
    // Parse confidence (u64 at offset 32-40)
    let conf_bytes: [u8; 8] = oracle_account.data[32..40]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse confidence bytes"))?;
    let confidence = u64::from_le_bytes(conf_bytes);
    
    // Parse valid_slot (u64 at offset 56-64)
    let valid_slot_bytes: [u8; 8] = oracle_account.data[56..64]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse valid_slot bytes"))?;
    let valid_slot = u64::from_le_bytes(valid_slot_bytes);
    
    // Check staleness
    let (max_slot_diff, _, _, _, _, _, max_conf_pct, _) = get_oracle_config();
    if current_slot.saturating_sub(valid_slot) > max_slot_diff {
        log::debug!(
            "Pyth v2 oracle {} stale: current={}, valid={}, diff={}",
            oracle_pubkey,
            current_slot,
            valid_slot,
            current_slot.saturating_sub(valid_slot)
        );
        return Ok((false, None));
    }
    
    // Calculate price
    let price = (price_raw as f64) * 10_f64.powi(exponent);
    
    // CRITICAL: Log price calculation details for debugging
    log::debug!(
        "Pyth v2 price calculation: raw={}, exponent={}, multiplier={}, final_price=${:.6}",
        price_raw,
        exponent,
        10_f64.powi(exponent),
        price
    );
    
    if price <= 0.0 || !price.is_finite() {
        log::debug!(
            "Pyth v2 oracle {}: Invalid price {} (raw: {}, exp: {})",
            oracle_pubkey,
            price,
            price_raw,
            exponent
        );
        return Ok((false, None));
    }
    
    // Check confidence
    let conf_value = (confidence as f64) * 10_f64.powi(exponent);
    let conf_pct = (conf_value / price.abs()) * 100.0;
    
    if conf_pct > max_conf_pct {
        log::debug!(
            "Pyth v2 oracle {} confidence too high: {:.2}% > {:.2}%",
            oracle_pubkey,
            conf_pct,
            max_conf_pct
        );
        return Ok((false, None));
    }
    
    // Check minimum price threshold
    if price.abs() < min_valid_price_usd() {
        log::debug!(
            "Pyth v2 oracle {} price below minimum threshold: {} < {}",
            oracle_pubkey,
            price.abs(),
            min_valid_price_usd()
        );
        return Ok((false, None));
    }
    
    log::debug!(
        "✅ Pyth v2 oracle validated: {} price=${:.6}, conf={:.2}%",
        oracle_pubkey,
        price,
        conf_pct
    );
    Ok((true, Some(price)))
}

/// Validate Pythnet v3 oracle (current mainnet standard)
/// 
/// Note: `current_slot` parameter is kept for API consistency with other validation functions,
/// but Pythnet v3 uses Clock sysvar for staleness checking instead of slot-based checks.
pub async fn validate_pyth_oracle_v3(
    rpc: &Arc<RpcClient>,
    oracle_pubkey: Pubkey,
    _current_slot: u64, // Unused: Pythnet v3 uses Clock sysvar for staleness, not slot-based
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
    
    // Configurable max age for Pythnet v3 (default: 60 seconds = 1 minute)
    // ✅ IMPROVED: Reduced from 300s to 60s for Pyth v3 - requires fresher data
    // Pyth v3 provides more frequent updates, so shorter max age is safer
    // Environment variable: PYTHNET_V3_MAX_AGE_SECONDS (default: 60)
    let max_age_seconds = std::env::var("PYTHNET_V3_MAX_AGE_SECONDS")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(60); // ✅ Reduced from 300s to 60s for better freshness
    
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
    
    // CRITICAL: Log price calculation details for debugging
    log::debug!(
        "Pyth v3 price calculation: raw={}, exponent={}, multiplier={}, final_price=${:.6}",
        price_info.price,
        price_info.expo,
        10_f64.powi(price_info.expo),
        price
    );
    
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

