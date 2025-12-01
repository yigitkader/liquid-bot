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
        
        if data.len() < 72 {
            log::error!("Switchboard account data too small: {} bytes (minimum 72 required)", data.len());
            return Err(anyhow::anyhow!("Switchboard account data too small: {} bytes", data.len()));
        }

        log::debug!("Switchboard account data structure: first 32 bytes = {:02x?}", &data[0..data.len().min(32)]);

        let mut cursor = 0;
        
        if data.len() >= 8 {
            cursor = 8;
            log::debug!("Skipping first 8 bytes (discriminator/anchor), cursor now at: {}", cursor);
        }

        if data.len() < cursor + 64 {
            log::error!("Switchboard account data incomplete: need {} bytes, have {} bytes", cursor + 64, data.len());
            return Err(anyhow::anyhow!("Switchboard account data incomplete: need {} bytes, have {}", cursor + 64, data.len()));
        }

        let price_bytes: [u8; 8] = data[cursor..cursor + 8]
            .try_into()
            .map_err(|e| {
                log::error!("Failed to read price bytes at offset {}: {:?}", cursor, e);
                anyhow::anyhow!("Failed to read price bytes")
            })?;
        let price = f64::from_le_bytes(price_bytes);
        log::debug!("Raw price bytes: {:02x?}, parsed as f64: {}", price_bytes, price);

        let confidence_bytes: [u8; 8] = data[cursor + 8..cursor + 16]
            .try_into()
            .map_err(|e| {
                log::error!("Failed to read confidence bytes at offset {}: {:?}", cursor + 8, e);
                anyhow::anyhow!("Failed to read confidence bytes")
            })?;
        let confidence = f64::from_le_bytes(confidence_bytes);
        log::debug!("Raw confidence bytes: {:02x?}, parsed as f64: {}", confidence_bytes, confidence);

        let timestamp_bytes: [u8; 8] = if data.len() >= cursor + 24 {
            data[cursor + 16..cursor + 24]
                .try_into()
                .map_err(|e| {
                    log::error!("Failed to read timestamp bytes at offset {}: {:?}", cursor + 16, e);
                    anyhow::anyhow!("Failed to read timestamp bytes")
                })?
        } else {
            log::warn!("Timestamp bytes not available (data length: {} < {}), using current time", data.len(), cursor + 24);
            [0u8; 8]
        };
        let timestamp = i64::from_le_bytes(timestamp_bytes);
        log::debug!("Raw timestamp bytes: {:02x?}, parsed as i64: {}", timestamp_bytes, timestamp);

        if price.is_infinite() || price.is_nan() {
            log::error!("Invalid price value: {} (infinite: {}, nan: {})", price, price.is_infinite(), price.is_nan());
            return Err(anyhow::anyhow!("Invalid price value: {}", price));
        }
        
        if price == 0.0 {
            log::warn!("⚠️  Switchboard oracle price is 0.0 - this might indicate:");
            log::warn!("   1. Parsing issue (wrong offset/structure)");
            log::warn!("   2. Empty or uninitialized oracle account");
            log::warn!("   3. Account data structure mismatch");
            log::warn!("   Account data size: {} bytes, cursor: {}, price bytes: {:02x?}", 
                data.len(), cursor, price_bytes);
        }

        let final_timestamp = if timestamp > 0 { 
            timestamp 
        } else { 
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| anyhow::anyhow!("Failed to get current time: {}", e))?
                .as_secs() as i64;
            log::warn!("Timestamp was 0, using current time: {}", now);
            now
        };

        log::info!("Switchboard oracle parsed: price=${:.4}, confidence=${:.4}, timestamp={}", 
            price, confidence, final_timestamp);

        Ok(PriceData {
            price,
            confidence: confidence.max(0.0),
            timestamp: final_timestamp,
        })
    }
}
