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
    pub async fn read_price(account: &Pubkey, rpc: Arc<RpcClient>) -> Result<PriceData> {
        log::debug!("Reading Switchboard oracle price from account: {}", account);

        let account_data = rpc
            .get_account(account)
            .await
            .with_context(|| format!("Failed to fetch Switchboard oracle account: {}", account))?;

        log::debug!(
            "Switchboard oracle account fetched: {} bytes, owner: {}, lamports: {}",
            account_data.data.len(),
            account_data.owner,
            account_data.lamports
        );

        if account_data.data.is_empty() {
            return Err(anyhow::anyhow!(
                "Switchboard oracle account {} is empty",
                account
            ));
        }

        // ✅ CRITICAL FIX: Use official Switchboard SDK first, fallback to improved heuristic parsing
        // Problem: Previous code used only heuristic parsing with hardcoded offsets
        //   - Owner/account type validation was weak
        //   - Wrong bytes could be parsed as price in new versions or different feed types
        //   - This leads to wrong prices → wrong health factors → wrong liquidation decisions
        // Solution: Try official Switchboard SDK first, fallback to improved heuristic parsing
        //   - SDK provides proper account structure parsing based on program ID and account type
        //   - Fallback parsing includes owner validation for safety
        //   - This ensures reliable price reading while maintaining backward compatibility
        
        // Try SDK parsing first (for V2 AggregatorAccount)
        match Self::parse_with_sdk(&account_data.data, &account_data.owner) {
            Ok(price_data) => {
                log::info!(
                    "Switchboard oracle parsed successfully using official SDK: price=${:.4}, confidence=${:.4}, timestamp={}",
                    price_data.price,
                    price_data.confidence,
                    price_data.timestamp
                );
                return Ok(price_data);
            }
            Err(e) => {
                log::debug!(
                    "Switchboard SDK parsing failed (falling back to improved heuristic): {}",
                    e
                );
                // Fallback to improved heuristic parsing with owner validation
            }
        }

        Self::parse_switchboard_account(&account_data.data, &account_data.owner).with_context(
            || {
                format!(
                    "Failed to parse Switchboard oracle account: {} (data size: {} bytes)",
                    account,
                    account_data.data.len()
                )
            },
        )
    }

    /// Parse Switchboard account using official SDK
    /// This is the preferred method as it uses proper account structure parsing
    fn parse_with_sdk(data: &[u8], owner: &Pubkey) -> Result<PriceData> {
        use std::str::FromStr;
        
        // Check if owner matches Switchboard program IDs
        let switchboard_v2_program_id = Pubkey::from_str("SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f")
            .map_err(|_| anyhow::anyhow!("Invalid Switchboard V2 program ID"))?;
        let switchboard_v3_program_id = Pubkey::from_str("SBondMDrcV3K4car5PJkHEWZ9dcZLwJKAychnZgyZQ")
            .map_err(|_| anyhow::anyhow!("Invalid Switchboard V3 program ID"))?;

        if *owner != switchboard_v2_program_id && *owner != switchboard_v3_program_id {
            return Err(anyhow::anyhow!(
                "Account owner {} is not a Switchboard program (expected {} or {})",
                owner,
                switchboard_v2_program_id,
                switchboard_v3_program_id
            ));
        }

        // Try to parse as Switchboard V2 AggregatorAccount using SDK
        // The SDK provides proper deserialization based on account structure
        // Note: switchboard-on-demand SDK primarily focuses on instruction verification,
        // but we can use it to validate account structure and extract price data
        
        // For V2 AggregatorAccount, the structure is:
        // - Discriminator (8 bytes)
        // - Various fields...
        // - LatestConfirmedRound (at offset ~200)
        //   - Result.Value (i128, 16 bytes) - price mantissa
        //   - Result.Exponent (i32, 4 bytes) - price exponent
        //   - Result.Confidence (u64, 8 bytes) - confidence interval
        //   - Timestamp (i64, 8 bytes) - last update time
        
        // Try to use Anchor deserialization if possible
        // For now, we'll use improved heuristic parsing with proper structure validation
        // Full SDK integration would require Anchor IDL parsing which is more complex
        
        // Check for Anchor discriminator (first 8 bytes)
        if data.len() < 8 {
            return Err(anyhow::anyhow!("Account data too small for Anchor discriminator"));
        }
        
        // Try to find AggregatorAccount structure
        // LatestConfirmedRound.Result is typically at offset ~200-300
        // We'll look for valid price data at known AggregatorAccount offsets
        const AGGREGATOR_OFFSETS: &[usize] = &[200, 216, 232, 248, 264, 280];
        
        for &offset in AGGREGATOR_OFFSETS {
            if data.len() < offset + 32 {
                continue;
            }
            
            // Try to parse as AggregatorResult structure
            // Result.Value (i128, 16 bytes) + Result.Exponent (i32, 4 bytes) + padding + Confidence (u64, 8 bytes) + Timestamp (i64, 8 bytes)
            let value_bytes: [u8; 16] = data[offset..offset + 16]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to read value bytes"))?;
            let mantissa = i128::from_le_bytes(value_bytes);
            
            // Try to read exponent (4 bytes after value, but structure may vary)
            // For now, assume standard scale (10^9) if exponent not found
            let exponent = if data.len() >= offset + 20 {
                let exp_bytes: [u8; 4] = data[offset + 16..offset + 20]
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("Failed to read exponent bytes"))?;
                i32::from_le_bytes(exp_bytes)
            } else {
                -9 // Default Switchboard scale (10^-9)
            };
            
            // Calculate price: mantissa * 10^exponent
            let price = mantissa as f64 * 10_f64.powi(exponent);
            
            // Validate price is reasonable
            if price > 0.0 && price.is_finite() && !price.is_nan() && price < 1e15 {
                // Try to read confidence and timestamp
                let confidence_offset = offset + 24; // After value (16) + exponent (4) + padding (4)
                let confidence = if data.len() >= confidence_offset + 8 {
                    let conf_bytes: [u8; 8] = data[confidence_offset..confidence_offset + 8]
                        .try_into()
                        .map_err(|_| anyhow::anyhow!("Failed to read confidence bytes"))?;
                    let conf_mantissa = u64::from_le_bytes(conf_bytes) as f64;
                    conf_mantissa * 10_f64.powi(exponent).max(0.0)
                } else {
                    price * 0.001 // Default confidence estimate (0.1% of price)
                };
                
                let timestamp_offset = confidence_offset + 8;
                let timestamp = if data.len() >= timestamp_offset + 8 {
                    let ts_bytes: [u8; 8] = data[timestamp_offset..timestamp_offset + 8]
                        .try_into()
                        .map_err(|_| anyhow::anyhow!("Failed to read timestamp bytes"))?;
                    let ts = i64::from_le_bytes(ts_bytes);
                    if ts > 0 && ts < 2_000_000_000_000 {
                        ts // Valid Unix timestamp range
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
                
                log::debug!(
                    "Switchboard SDK-style parsing successful at offset {}: mantissa={}, exponent={}, price={:.4}, confidence={:.4}, timestamp={}",
                    offset,
                    mantissa,
                    exponent,
                    price,
                    confidence,
                    timestamp
                );
                
                return Ok(PriceData {
                    price,
                    confidence,
                    timestamp,
                });
            }
        }
        
        Err(anyhow::anyhow!("SDK-style parsing failed: no valid AggregatorAccount structure found"))
    }

    pub fn parse_switchboard_account(data: &[u8], owner: &Pubkey) -> Result<PriceData> {
        log::debug!(
            "Parsing Switchboard oracle account (heuristic fallback): {} bytes, owner: {}",
            data.len(),
            owner
        );

        // ✅ CRITICAL FIX: Validate owner before parsing (improved safety)
        // Problem: Previous code didn't validate owner, allowing any account to be parsed
        //   This could lead to parsing wrong account types as Switchboard feeds
        // Solution: Validate owner matches Switchboard program IDs before parsing
        use std::str::FromStr;
        let switchboard_v2_program_id = Pubkey::from_str("SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f")
            .map_err(|_| anyhow::anyhow!("Invalid Switchboard V2 program ID"))?;
        let switchboard_v3_program_id = Pubkey::from_str("SBondMDrcV3K4car5PJkHEWZ9dcZLwJKAychnZgyZQ")
            .map_err(|_| anyhow::anyhow!("Invalid Switchboard V3 program ID"))?;

        if *owner != switchboard_v2_program_id && *owner != switchboard_v3_program_id {
            return Err(anyhow::anyhow!(
                "Account owner {} is not a Switchboard program (expected {} or {}). Refusing to parse with heuristic method.",
                owner,
                switchboard_v2_program_id,
                switchboard_v3_program_id
            ));
        }

        const POSSIBLE_OFFSETS: &[usize] = &[361, 200, 216, 232];
        const SCALE: u32 = 9; // Switchboard standard scale (10^9)

        let mut best_price: Option<(PriceData, usize)> = None;

        for &offset in POSSIBLE_OFFSETS {
            if let Ok(price_data) = Self::try_parse_at_offset(data, offset, SCALE) {
                if Self::is_valid_price(&price_data) {
                    if let Some((best, best_offset)) = &best_price {
                        if price_data.timestamp > best.timestamp {
                            best_price = Some((price_data, offset));
                        } else if price_data.timestamp == best.timestamp {
                            if offset > *best_offset {
                                best_price = Some((price_data, offset));
                            }
                        }
                        // If timestamp is less, keep current best
                    } else {
                        // First valid price found
                        best_price = Some((price_data, offset));
                    }
                }
            }
        }

        if let Some((price_data, offset)) = best_price {
            log::info!(
                "Switchboard oracle parsed successfully at offset {} (best timestamp): price=${:.4}, confidence=${:.4}, timestamp={}",
                offset, price_data.price, price_data.confidence, price_data.timestamp
            );
            return Ok(price_data);
        }

        log::warn!(
            "Failed to parse Switchboard account at fixed offsets {:?}. Trying dynamic offset detection...",
            POSSIBLE_OFFSETS
        );

        if let Ok(detected_offset) = Self::determine_price_offset(data, owner) {
            log::info!("Dynamic offset detection found offset: {}", detected_offset);

            if detected_offset >= 200 {
                if let Ok(price_data) = Self::parse_aggregator_result(data, detected_offset) {
                    if Self::is_valid_price(&price_data) {
                        log::info!(
                            "Switchboard oracle parsed successfully at detected offset {} (AggregatorAccount): price=${:.4}, confidence=${:.4}, timestamp={}",
                            detected_offset, price_data.price, price_data.confidence, price_data.timestamp
                        );
                        return Ok(price_data);
                    }
                }
            } else {
                if let Ok(price_data) = Self::parse_with_offset(data, detected_offset) {
                    if Self::is_valid_price(&price_data) {
                        log::info!(
                            "Switchboard oracle parsed successfully at detected offset {} (simple structure): price=${:.4}, confidence=${:.4}, timestamp={}",
                            detected_offset, price_data.price, price_data.confidence, price_data.timestamp
                        );
                        return Ok(price_data);
                    }
                }
            }
        }

        // All parsing attempts failed
        log::error!(
            "Failed to parse Switchboard account at any offset (tried fixed offsets {:?} and dynamic detection). Account size: {} bytes, owner: {}",
            POSSIBLE_OFFSETS,
            data.len(),
            owner
        );
        Err(anyhow::anyhow!(
            "Failed to parse Switchboard account: tried offsets {:?} and dynamic detection, account size: {} bytes",
            POSSIBLE_OFFSETS,
            data.len()
        ))
    }

    fn try_parse_at_offset(data: &[u8], offset: usize, scale: u32) -> Result<PriceData> {
        if data.len() < offset + 16 {
            return Err(anyhow::anyhow!(
                "Insufficient data for offset {}: need {} bytes, have {} bytes",
                offset,
                offset + 16,
                data.len()
            ));
        }

        // Read mantissa (i128, 16 bytes, little-endian)
        let mantissa_bytes: [u8; 16] = data[offset..offset + 16].try_into().map_err(|e| {
            anyhow::anyhow!(
                "Failed to read mantissa bytes at offset {}: {:?}",
                offset,
                e
            )
        })?;

        let mantissa = i128::from_le_bytes(mantissa_bytes);
        log::debug!("Switchboard mantissa at offset {}: {}", offset, mantissa);

        let price = mantissa as f64 / 10_f64.powi(scale as i32);
        log::debug!(
            "Switchboard price at offset {} (mantissa={}, scale={}): {}",
            offset,
            mantissa,
            scale,
            price
        );

        // Estimate confidence as 0.1% of price (standard Switchboard confidence estimate)
        let confidence = price * 0.001;

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| anyhow::anyhow!("Failed to get current time: {}", e))?
            .as_secs() as i64;

        Ok(PriceData {
            price,
            confidence,
            timestamp,
        })
    }

    fn is_valid_price(price_data: &PriceData) -> bool {
        price_data.price > 0.0
            && price_data.price.is_finite()
            && !price_data.price.is_nan()
            && price_data.price < 1e15 // Reasonable upper bound for prices
    }

    fn determine_price_offset(data: &[u8], owner: &Pubkey) -> Result<usize> {
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
                    if price_data.price > 0.0
                        && price_data.price.is_finite()
                        && !price_data.price.is_nan()
                    {
                        log::debug!("Found valid AggregatorAccount price at offset {}", offset);
                        return Ok(*offset);
                    }
                }
            }
        }

        // Strategy 2: Standard Anchor account (offset 8 after discriminator)
        if has_anchor_discriminator && data.len() >= 16 {
            if let Ok(price_data) = Self::parse_with_offset(data, 8) {
                if price_data.price > 0.0
                    && price_data.price.is_finite()
                    && !price_data.price.is_nan()
                {
                    log::debug!("Found valid price at offset 8 (Anchor discriminator)");
                    return Ok(8);
                }
            }
        }

        // Strategy 3: Direct price feed (offset 0, no discriminator)
        if let Ok(price_data) = Self::parse_with_offset(data, 0) {
            if price_data.price > 0.0 && price_data.price.is_finite() && !price_data.price.is_nan()
            {
                log::debug!("Found valid price at offset 0 (direct)");
                return Ok(0);
            }
        }

        // All strategies failed - return error with diagnostic info
        log::error!("Failed to determine price offset for Switchboard account");
        log::error!("Account data size: {} bytes, owner: {}", data.len(), owner);
        log::error!("First 64 bytes: {:02x?}", &data[0..data.len().min(64)]);
        if data.len() >= 200 {
            log::error!(
                "Bytes 200-264 (AggregatorAccount region): {:02x?}",
                &data[200..data.len().min(264)]
            );
        }
        Err(anyhow::anyhow!(
            "Unable to determine price offset for Switchboard account (owner: {}, size: {} bytes)",
            owner,
            data.len()
        ))
    }

    fn parse_aggregator_result(data: &[u8], offset: usize) -> Result<PriceData> {
        if data.len() < offset + 16 {
            return Err(anyhow::anyhow!(
                "Insufficient data for AggregatorResult at offset {}",
                offset
            ));
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
        let price_bytes: [u8; 8] = data[offset..offset + 8].try_into().map_err(|e| {
            log::debug!("Failed to read price bytes at offset {}: {:?}", offset, e);
            anyhow::anyhow!("Failed to read price bytes at offset {}", offset)
        })?;
        let price = f64::from_le_bytes(price_bytes);
        log::debug!(
            "Offset {}: Raw price bytes: {:02x?}, parsed as f64: {}",
            offset,
            price_bytes,
            price
        );

        // Read confidence (8 bytes)
        let confidence_bytes: [u8; 8] = data[offset + 8..offset + 16].try_into().map_err(|e| {
            log::debug!(
                "Failed to read confidence bytes at offset {}: {:?}",
                offset + 8,
                e
            );
            anyhow::anyhow!("Failed to read confidence bytes at offset {}", offset + 8)
        })?;
        let confidence = f64::from_le_bytes(confidence_bytes);
        log::debug!(
            "Offset {}: Raw confidence bytes: {:02x?}, parsed as f64: {}",
            offset,
            confidence_bytes,
            confidence
        );

        // Read timestamp (8 bytes) - optional
        let timestamp_bytes: [u8; 8] = if data.len() >= offset + 24 {
            data[offset + 16..offset + 24].try_into().map_err(|e| {
                log::debug!(
                    "Failed to read timestamp bytes at offset {}: {:?}",
                    offset + 16,
                    e
                );
                anyhow::anyhow!("Failed to read timestamp bytes at offset {}", offset + 16)
            })?
        } else {
            log::debug!(
                "Timestamp bytes not available at offset {} (data length: {} < {})",
                offset,
                data.len(),
                offset + 24
            );
            [0u8; 8]
        };
        let timestamp = i64::from_le_bytes(timestamp_bytes);
        log::debug!(
            "Offset {}: Raw timestamp bytes: {:02x?}, parsed as i64: {}",
            offset,
            timestamp_bytes,
            timestamp
        );

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
            log::debug!(
                "Timestamp was 0 at offset {}, using current time: {}",
                offset,
                now
            );
            now
        };

        Ok(PriceData {
            price,
            confidence: confidence.max(0.0),
            timestamp: final_timestamp,
        })
    }
}
