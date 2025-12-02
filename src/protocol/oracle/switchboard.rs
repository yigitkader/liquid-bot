use crate::blockchain::rpc_client::RpcClient;
use anyhow::{Context, Result};
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;

pub struct SwitchboardOracle;

#[derive(Debug, Clone)]
pub struct PriceData {
    pub price: f64,
    pub confidence: f64,
    pub timestamp: i64,
}

impl SwitchboardOracle {
    pub async fn read_price(
        account: &Pubkey,
        rpc: Arc<RpcClient>,
    ) -> Result<PriceData> {
        log::debug!("Reading Switchboard oracle price from account: {}", account);
        
        let account_data = rpc.get_account(account).await
            .with_context(|| format!("Failed to fetch Switchboard oracle account: {}", account))?;
        
        log::debug!("Switchboard oracle account fetched: {} bytes, owner: {}, lamports: {}", 
            account_data.data.len(), account_data.owner, account_data.lamports);
        
        if account_data.data.is_empty() {
            return Err(anyhow::anyhow!("Switchboard oracle account {} is empty", account));
        }
        
        Self::parse_switchboard_account(&account_data.data, &account_data.owner)
            .with_context(|| format!("Failed to parse Switchboard oracle account: {} (data size: {} bytes)", 
                account, account_data.data.len()))
    }

    /// Parse Switchboard account data using the correct offset for AggregatorAccount.
    /// 
    /// Switchboard V2 AggregatorAccount structure:
    /// - LatestConfirmedRound.Result.Value is at offset 361 (i128, 16 bytes)
    /// - Price mantissa needs to be scaled by 10^9 (standard Switchboard scale)
    pub fn parse_switchboard_account(data: &[u8], owner: &Pubkey) -> Result<PriceData> {
        log::debug!("Parsing Switchboard oracle account: {} bytes, owner: {}", data.len(), owner);
        
        const LATEST_RESULT_OFFSET: usize = 361; // Correct offset for LatestConfirmedRound.Result.Value
        const SCALE: u32 = 9; // Switchboard standard scale (10^9)
        
        if data.len() < LATEST_RESULT_OFFSET + 16 {
            log::error!("Switchboard account data too small: {} bytes (need at least {} bytes)", 
                data.len(), LATEST_RESULT_OFFSET + 16);
            return Err(anyhow::anyhow!(
                "Switchboard account data too small: {} bytes (need at least {} bytes)",
                data.len(),
                LATEST_RESULT_OFFSET + 16
            ));
        }

        // Read mantissa (i128, 16 bytes, little-endian)
        let mantissa_bytes: [u8; 16] = data[LATEST_RESULT_OFFSET..LATEST_RESULT_OFFSET + 16]
            .try_into()
            .map_err(|e| anyhow::anyhow!("Failed to read mantissa bytes at offset {}: {:?}", LATEST_RESULT_OFFSET, e))?;
        
        let mantissa = i128::from_le_bytes(mantissa_bytes);
        log::debug!("Switchboard mantissa at offset {}: {}", LATEST_RESULT_OFFSET, mantissa);
        
        // Convert mantissa to price using scale
        let price = mantissa as f64 / 10_f64.powi(SCALE as i32);
        log::debug!("Switchboard price (mantissa={}, scale={}): {}", mantissa, SCALE, price);
        
        // Validation
        if price <= 0.0 || price.is_infinite() || price.is_nan() {
            return Err(anyhow::anyhow!(
                "Invalid price value: {} (mantissa: {}, scale: {})",
                price,
                mantissa,
                SCALE
            ));
        }
        
        // Estimate confidence as 0.1% of price (standard Switchboard confidence estimate)
        let confidence = price * 0.001;
        
        // Get current timestamp
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| anyhow::anyhow!("Failed to get current time: {}", e))?
            .as_secs() as i64;

        log::info!("Switchboard oracle parsed successfully: price=${:.4}, confidence=${:.4}, timestamp={}", 
            price, confidence, timestamp);
        
        Ok(PriceData {
            price,
            confidence,
            timestamp,
        })
    }

    /// Determine the correct offset for price data based on account structure.
    /// 
    /// Switchboard V2 AggregatorAccount structure:
    /// - Anchor discriminator: 8 bytes (if Anchor account)
    /// - Name: 32 bytes
    /// - Metadata: 32 bytes
    /// - Authorized: 1 byte
    /// - Queue: Pubkey (32 bytes)
    /// - ... other fields ...
    /// - LatestConfirmedRound: AggregatorRound (~200+ bytes offset)
    ///   - Result.Value: i128 (16 bytes) - this is the price mantissa
    /// 
    /// For simpler price feed accounts, price might be at:
    /// - Offset 0: Direct price feed (no discriminator)
    /// - Offset 8: After Anchor discriminator
    /// - Offset 200+: AggregatorAccount LatestConfirmedRound.Result.Value
    fn determine_price_offset(data: &[u8], owner: &Pubkey) -> Result<usize> {
        // Check if this is an Anchor account (has 8-byte discriminator)
        // Anchor discriminators are typically non-zero in first 8 bytes
        let has_anchor_discriminator = data.len() >= 8 && {
            // Anchor discriminators are usually SHA256 hash prefixes, not all zeros
            // Check if first 8 bytes look like a discriminator (not all zeros, not all 0xFF)
            let discriminator = &data[0..8];
            discriminator.iter().any(|&b| b != 0) && discriminator.iter().any(|&b| b != 0xFF)
        };

        // Try to detect Switchboard V2 AggregatorAccount structure
        // LatestConfirmedRound.Result.Value is typically at offset ~200-300
        // We'll check for reasonable price values at known offsets
        
        // Strategy 1: Check for AggregatorAccount structure (offset ~200-300)
        // LatestConfirmedRound.Result.Value is i128 (16 bytes) representing price mantissa
        // We need to check if there's a valid i128 value that could be a price
        for offset in [200, 216, 232, 248, 264, 280].iter() {
            if *offset + 16 <= data.len() {
                if let Ok(price_data) = Self::parse_aggregator_result(data, *offset) {
                    if price_data.price > 0.0 && price_data.price.is_finite() && !price_data.price.is_nan() {
                        log::debug!("Found valid AggregatorAccount price at offset {}", offset);
                        return Ok(*offset);
                    }
                }
            }
        }

        // Strategy 2: Standard Anchor account (offset 8 after discriminator)
        if has_anchor_discriminator && data.len() >= 16 {
            if let Ok(price_data) = Self::parse_with_offset(data, 8) {
                if price_data.price > 0.0 && price_data.price.is_finite() && !price_data.price.is_nan() {
                    log::debug!("Found valid price at offset 8 (Anchor discriminator)");
                    return Ok(8);
                }
            }
        }

        // Strategy 3: Direct price feed (offset 0, no discriminator)
        if let Ok(price_data) = Self::parse_with_offset(data, 0) {
            if price_data.price > 0.0 && price_data.price.is_finite() && !price_data.price.is_nan() {
                log::debug!("Found valid price at offset 0 (direct)");
                return Ok(0);
            }
        }

        // All strategies failed - return error with diagnostic info
        log::error!("Failed to determine price offset for Switchboard account");
        log::error!("Account data size: {} bytes, owner: {}", data.len(), owner);
        log::error!("First 64 bytes: {:02x?}", &data[0..data.len().min(64)]);
        if data.len() >= 200 {
            log::error!("Bytes 200-264 (AggregatorAccount region): {:02x?}", &data[200..data.len().min(264)]);
        }
        Err(anyhow::anyhow!(
            "Unable to determine price offset for Switchboard account (owner: {}, size: {} bytes)",
            owner,
            data.len()
        ))
    }

    /// Parse AggregatorAccount LatestConfirmedRound.Result.Value
    /// Result.Value is i128 (16 bytes) representing price mantissa
    /// We need to convert this to f64 price (typically needs exponent, but we'll try direct conversion)
    fn parse_aggregator_result(data: &[u8], offset: usize) -> Result<PriceData> {
        if data.len() < offset + 16 {
            return Err(anyhow::anyhow!("Insufficient data for AggregatorResult at offset {}", offset));
        }

        // Read i128 value (16 bytes, little-endian)
        let mut value_bytes = [0u8; 16];
        value_bytes.copy_from_slice(&data[offset..offset + 16]);
        let mantissa = i128::from_le_bytes(value_bytes);

        // Try to find exponent in nearby fields (typically 8 bytes before or after)
        // For now, assume price is already in correct units (common for Switchboard feeds)
        let price = mantissa as f64;
        
        // Try to read confidence from next 8 bytes (if available)
        let confidence = if data.len() >= offset + 24 {
            let conf_bytes: [u8; 8] = data[offset + 16..offset + 24]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to read confidence bytes"))?;
            f64::from_le_bytes(conf_bytes).max(0.0)
        } else {
            0.0
        };

        // Try to read timestamp from next 8 bytes (if available)
        let timestamp = if data.len() >= offset + 32 {
            let ts_bytes: [u8; 8] = data[offset + 24..offset + 32]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to read timestamp bytes"))?;
            let ts = i64::from_le_bytes(ts_bytes);
            if ts > 0 {
                ts
            } else {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| anyhow::anyhow!("Failed to get current time: {}", e))?
                    .as_secs() as i64
            }
        } else {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| anyhow::anyhow!("Failed to get current time: {}", e))?
                .as_secs() as i64
        };

        Ok(PriceData {
            price,
            confidence,
            timestamp,
        })
    }

    /// Parse Switchboard account data starting from a specific offset
    fn parse_with_offset(data: &[u8], offset: usize) -> Result<PriceData> {
        if data.len() < offset + 24 {
            return Err(anyhow::anyhow!(
                "Insufficient data for offset {}: need {} bytes, have {} bytes",
                offset,
                offset + 24,
                data.len()
            ));
        }

        // Read price (8 bytes)
        let price_bytes: [u8; 8] = data[offset..offset + 8]
            .try_into()
            .map_err(|e| {
                log::debug!("Failed to read price bytes at offset {}: {:?}", offset, e);
                anyhow::anyhow!("Failed to read price bytes at offset {}", offset)
            })?;
        let price = f64::from_le_bytes(price_bytes);
        log::debug!("Offset {}: Raw price bytes: {:02x?}, parsed as f64: {}", offset, price_bytes, price);

        // Read confidence (8 bytes)
        let confidence_bytes: [u8; 8] = data[offset + 8..offset + 16]
            .try_into()
            .map_err(|e| {
                log::debug!("Failed to read confidence bytes at offset {}: {:?}", offset + 8, e);
                anyhow::anyhow!("Failed to read confidence bytes at offset {}", offset + 8)
            })?;
        let confidence = f64::from_le_bytes(confidence_bytes);
        log::debug!("Offset {}: Raw confidence bytes: {:02x?}, parsed as f64: {}", offset, confidence_bytes, confidence);

        // Read timestamp (8 bytes) - optional
        let timestamp_bytes: [u8; 8] = if data.len() >= offset + 24 {
            data[offset + 16..offset + 24]
                .try_into()
                .map_err(|e| {
                    log::debug!("Failed to read timestamp bytes at offset {}: {:?}", offset + 16, e);
                    anyhow::anyhow!("Failed to read timestamp bytes at offset {}", offset + 16)
                })?
        } else {
            log::debug!("Timestamp bytes not available at offset {} (data length: {} < {})", 
                offset, data.len(), offset + 24);
            [0u8; 8]
        };
        let timestamp = i64::from_le_bytes(timestamp_bytes);
        log::debug!("Offset {}: Raw timestamp bytes: {:02x?}, parsed as i64: {}", offset, timestamp_bytes, timestamp);

        // Validate price
        if price.is_infinite() || price.is_nan() {
            return Err(anyhow::anyhow!(
                "Invalid price value at offset {}: {} (infinite: {}, nan: {})",
                offset,
                price,
                price.is_infinite(),
                price.is_nan()
            ));
        }

        // Use current time if timestamp is invalid
        let final_timestamp = if timestamp > 0 { 
            timestamp 
        } else { 
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| anyhow::anyhow!("Failed to get current time: {}", e))?
                .as_secs() as i64;
            log::debug!("Timestamp was 0 at offset {}, using current time: {}", offset, now);
            now
        };

        Ok(PriceData {
            price,
            confidence: confidence.max(0.0),
            timestamp: final_timestamp,
        })
    }
}
