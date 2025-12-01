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
        
        Self::parse_switchboard_account(&account_data.data)
            .with_context(|| format!("Failed to parse Switchboard oracle account: {} (data size: {} bytes)", 
                account, account_data.data.len()))
    }

    pub fn parse_switchboard_account(data: &[u8]) -> Result<PriceData> {
        log::debug!("Parsing Switchboard oracle account: {} bytes", data.len());
        
        if data.len() < 24 {
            log::error!("Switchboard account data too small: {} bytes (minimum 24 required)", data.len());
            return Err(anyhow::anyhow!("Switchboard account data too small: {} bytes", data.len()));
        }

        log::debug!("Switchboard account data structure: first 32 bytes = {:02x?}", &data[0..data.len().min(32)]);

        // Try multiple parsing strategies in order of preference
        // Strategy 1: Offset 8 (most common - Anchor discriminator)
        if let Ok(price_data) = Self::parse_with_offset(data, 8) {
            if price_data.price > 0.0 && price_data.price.is_finite() && !price_data.price.is_nan() {
                log::info!("Switchboard oracle parsed successfully with offset 8: price=${:.4}, confidence=${:.4}, timestamp={}", 
                    price_data.price, price_data.confidence, price_data.timestamp);
                return Ok(price_data);
            } else {
                log::warn!("Switchboard parsing with offset 8 returned invalid price: {}", price_data.price);
            }
        }

        // Strategy 2: Offset 0 (no discriminator - direct parsing)
        if let Ok(price_data) = Self::parse_with_offset(data, 0) {
            if price_data.price > 0.0 && price_data.price.is_finite() && !price_data.price.is_nan() {
                log::info!("Switchboard oracle parsed successfully with offset 0: price=${:.4}, confidence=${:.4}, timestamp={}", 
                    price_data.price, price_data.confidence, price_data.timestamp);
                return Ok(price_data);
            } else {
                log::warn!("Switchboard parsing with offset 0 returned invalid price: {}", price_data.price);
            }
        }

        // Strategy 3: Try offset 16 (alternative structure)
        if data.len() >= 40 {
            if let Ok(price_data) = Self::parse_with_offset(data, 16) {
                if price_data.price > 0.0 && price_data.price.is_finite() && !price_data.price.is_nan() {
                    log::info!("Switchboard oracle parsed successfully with offset 16: price=${:.4}, confidence=${:.4}, timestamp={}", 
                        price_data.price, price_data.confidence, price_data.timestamp);
                    return Ok(price_data);
                }
            }
        }

        // All strategies failed
        log::error!("All Switchboard parsing strategies failed");
        log::error!("Account data size: {} bytes", data.len());
        log::error!("First 64 bytes: {:02x?}", &data[0..data.len().min(64)]);
        Err(anyhow::anyhow!("All Switchboard parsing strategies failed - unable to parse oracle account"))
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
