// Switchboard Oracle Module
// Handles Switchboard v2 and v3 (On-Demand) oracle validation

use anyhow::Result;
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use std::sync::Arc;

#[allow(dead_code)]

// Helper functions for aligned buffer pool (shared with pipeline.rs)
// These are used for Switchboard Oracle alignment fix
use parking_lot::Mutex;
use lazy_static::lazy_static;

lazy_static! {
    static ref ALIGNED_BUFFERS: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());
}

fn get_aligned_buffer(size: usize) -> Vec<u8> {
    let mut pool = ALIGNED_BUFFERS.lock();
    match pool.pop() {
        Some(mut buffer) => {
            if buffer.capacity() < size {
                buffer = vec![0u8; size];
            } else {
                buffer.resize(size, 0u8);
            }
            buffer
        }
        None => vec![0u8; size],
    }
}

fn return_aligned_buffer(mut buffer: Vec<u8>) {
    buffer.clear();
    let mut pool = ALIGNED_BUFFERS.lock();
    if pool.len() < 10 {
        pool.push(buffer);
    }
}

/// Get Switchboard v2 program ID from environment
pub fn switchboard_program_id_v2() -> Result<Pubkey> {
    use std::env;
    let switchboard_id_str = env::var("SWITCHBOARD_PROGRAM_ID")
        .map_err(|_| anyhow::anyhow!(
            "SWITCHBOARD_PROGRAM_ID not found in .env file. \
             Please set SWITCHBOARD_PROGRAM_ID in .env file. \
             Example: SWITCHBOARD_PROGRAM_ID=SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f"
        ))?;
    Pubkey::from_str(&switchboard_id_str)
        .map_err(|e| anyhow::anyhow!("Invalid SWITCHBOARD_PROGRAM_ID from .env: {} - Error: {}", switchboard_id_str, e))
}

/// Get Switchboard On-Demand v3 program ID from environment
pub fn switchboard_program_id_v3() -> Result<Pubkey> {
    use std::env;
    let switchboard_id_str = env::var("SWITCHBOARD_PROGRAM_ID_V3")
        .map_err(|_| anyhow::anyhow!(
            "SWITCHBOARD_PROGRAM_ID_V3 not found in .env file. \
             Please set SWITCHBOARD_PROGRAM_ID_V3 in .env file. \
             Example: SWITCHBOARD_PROGRAM_ID_V3=SBondMDrcV3K4kxZR1HNVT7osZxAHVHgYXL5Ze1oMUv"
        ))?;
    Pubkey::from_str(&switchboard_id_str)
        .map_err(|e| anyhow::anyhow!("Invalid SWITCHBOARD_PROGRAM_ID_V3 from .env: {} - Error: {}", switchboard_id_str, e))
}

/// Validate Switchboard oracle if available in ReserveConfig
/// Returns Some(price) if Switchboard oracle exists and is valid, None otherwise
pub async fn validate_switchboard_oracle_if_available(
    rpc: &Arc<RpcClient>,
    reserve: &crate::kamino::Reserve,
    current_slot: u64,
) -> Result<Option<f64>> {
    // Use Switchboard oracle from Kamino Reserve
    let switchboard_oracle_pubkey = reserve.switchboard_oracle();
    if switchboard_oracle_pubkey == Pubkey::default() {
        // No Switchboard oracle configured for this reserve
        return Ok(None);
    }

    // Delegate to validate_switchboard_oracle_by_pubkey
    validate_switchboard_oracle_by_pubkey(rpc, switchboard_oracle_pubkey, current_slot).await
}

/// Validate Switchboard oracle by pubkey (standalone version)
/// This is the core validation logic shared by both validate_switchboard_oracle_if_available
/// and direct pubkey validation
pub async fn validate_switchboard_oracle_by_pubkey(
    rpc: &Arc<RpcClient>,
    switchboard_oracle_pubkey: Pubkey,
    current_slot: u64,
) -> Result<Option<f64>> {
    use switchboard_on_demand::on_demand::accounts::pull_feed::PullFeedAccountData;
    
    // Get Switchboard feed account data from chain
    let oracle_account = rpc
        .get_account(&switchboard_oracle_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get Switchboard oracle account: {}", e))?;

    // Check for both Switchboard v2 and v3 program IDs
    let switchboard_program_id_v2 = switchboard_program_id_v2()?;
    let switchboard_program_id_v3 = switchboard_program_id_v3()?;

    // CRITICAL SECURITY: Verify account owner BEFORE parsing
    if oracle_account.owner != switchboard_program_id_v2 
        && oracle_account.owner != switchboard_program_id_v3 {
        log::debug!(
            "Switchboard oracle account {} rejected: invalid owner (expected v2: {} or v3: {}, found: {})",
            switchboard_oracle_pubkey,
            switchboard_program_id_v2,
            switchboard_program_id_v3,
            oracle_account.owner
        );
        return Ok(None);
    }
    
    // Size check with realistic range
    const EXPECTED_FEED_SIZE_MIN: usize = 200;
    const EXPECTED_FEED_SIZE_MAX: usize = 10000;
    let account_size = oracle_account.data.len();
    
    if account_size < EXPECTED_FEED_SIZE_MIN {
        log::debug!("Switchboard oracle {} too small: {} bytes", switchboard_oracle_pubkey, account_size);
        return Ok(None);
    }
    
    if account_size > EXPECTED_FEED_SIZE_MAX {
        log::warn!(
            "⚠️ Switchboard oracle {} unusually large: {} bytes (expected max {}). Attempting to parse anyway...",
            switchboard_oracle_pubkey,
            account_size,
            EXPECTED_FEED_SIZE_MAX
        );
    }

    // Route to v2 or v3 parser based on owner
    if oracle_account.owner == switchboard_program_id_v3 {
        // ========== SWITCHBOARD v3 (On-Demand) ==========
        // Use PullFeedAccountData (Anchor format with discriminator)
        
        const DISCRIMINATOR_SIZE: usize = 8;
        if oracle_account.data.len() < DISCRIMINATOR_SIZE {
            return Ok(None);
        }
        
        // Validate discriminator
        use switchboard_on_demand::utils::get_account_discriminator;
        let expected_discriminator = get_account_discriminator("PullFeedAccountData");
        let actual_discriminator: [u8; 8] = oracle_account.data[0..8]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to extract discriminator"))?;
        
        if actual_discriminator != expected_discriminator {
            log::debug!("Switchboard v3 account {} has invalid discriminator", switchboard_oracle_pubkey);
            return Ok(None);
        }
        
        // Parse struct data (skip discriminator)
        let feed_size = std::mem::size_of::<PullFeedAccountData>();
        let required_total_size = DISCRIMINATOR_SIZE + feed_size;
        if oracle_account.data.len() < required_total_size {
            return Ok(None);
        }
        
        let struct_data = &oracle_account.data[DISCRIMINATOR_SIZE..DISCRIMINATOR_SIZE + feed_size];
        
        // Parse using bytemuck with alignment handling
        let feed = match bytemuck::try_from_bytes::<PullFeedAccountData>(struct_data) {
            Ok(feed) => *feed,
            Err(bytemuck::PodCastError::TargetAlignmentGreaterAndInputNotAligned) => {
                // Alignment issue - use helper functions for aligned buffer
                let alignment = std::mem::align_of::<PullFeedAccountData>();
                let mut aligned_buffer = get_aligned_buffer(feed_size + alignment);
                let ptr = aligned_buffer.as_ptr();
                let offset = ptr.align_offset(alignment);
                aligned_buffer[offset..offset + feed_size].copy_from_slice(struct_data);
                
                match bytemuck::try_from_bytes::<PullFeedAccountData>(&aligned_buffer[offset..offset + feed_size]) {
                    Ok(feed) => {
                        let feed_copy = *feed;
                        return_aligned_buffer(aligned_buffer);
                        feed_copy
                    }
                    Err(_) => {
                        return_aligned_buffer(aligned_buffer);
                        log::debug!("Failed to parse Switchboard v3 feed {}: alignment/parsing error", switchboard_oracle_pubkey);
                        return Ok(None);
                    }
                }
            }
            Err(_) => {
                log::debug!("Failed to parse Switchboard v3 feed {}: parsing error", switchboard_oracle_pubkey);
                return Ok(None);
            }
        };
        
        // Get price using SDK's value() method
        match feed.value(current_slot) {
            Ok(price_decimal) => {
                // Convert Decimal to f64
                let price = price_decimal.to_string().parse::<f64>()
                    .unwrap_or_else(|_| {
                        let mantissa = price_decimal.mantissa();
                        let scale = price_decimal.scale();
                        mantissa as f64 / 10_f64.powi(scale as i32)
                    });
                
                if price > 0.0 && price.is_finite() {
                    Ok(Some(price))
                } else {
                    Ok(None)
                }
            }
            Err(_) => {
                log::debug!("Switchboard v3 feed {} value() failed (stale or insufficient quorum)", switchboard_oracle_pubkey);
                Ok(None)
            }
        }
    } else if oracle_account.owner == switchboard_program_id_v2 {
        // ========== SWITCHBOARD v2 ==========
        // Parse latest_round.result (SwitchboardDecimal)
        const LATEST_ROUND_OFFSET: usize = 80;
        const RESULT_OFFSET: usize = LATEST_ROUND_OFFSET + 24; // 104
        
        if oracle_account.data.len() < RESULT_OFFSET + 20 {
            return Ok(None);
        }
        
        // Parse mantissa (i128, 16 bytes) and scale (u32, 4 bytes)
        let mantissa_bytes: [u8; 16] = oracle_account.data[RESULT_OFFSET..RESULT_OFFSET + 16]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to parse mantissa"))?;
        let mantissa = i128::from_le_bytes(mantissa_bytes);
        
        let scale_bytes: [u8; 4] = oracle_account.data[RESULT_OFFSET + 16..RESULT_OFFSET + 20]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to parse scale"))?;
        let scale = u32::from_le_bytes(scale_bytes);
        
        if scale > 18 {
            return Ok(None);
        }
        
        let price_multiplier = 10_f64.powi(scale as i32);
        if !price_multiplier.is_finite() {
            return Ok(None);
        }
        
        let price = mantissa as f64 / price_multiplier;
        if price > 0.0 && price.is_finite() {
            Ok(Some(price))
        } else {
            Ok(None)
        }
    } else {
        Ok(None)
    }
}

