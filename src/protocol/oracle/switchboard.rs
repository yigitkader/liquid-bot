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
        let account_data = rpc.get_account(account).await?;
        Self::parse_switchboard_account(&account_data.data)
            .context("Failed to parse Switchboard oracle account")
    }

    pub fn parse_switchboard_account(data: &[u8]) -> Result<PriceData> {
        if data.len() < 72 {
            return Err(anyhow::anyhow!("Switchboard account data too small: {} bytes", data.len()));
        }

        let mut cursor = 0;
        
        if data.len() >= 8 {
            cursor = 8;
        }

        if data.len() < cursor + 64 {
            return Err(anyhow::anyhow!("Switchboard account data incomplete"));
        }

        let price_bytes: [u8; 8] = data[cursor..cursor + 8]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to read price bytes"))?;
        let price = f64::from_le_bytes(price_bytes);

        let confidence_bytes: [u8; 8] = data[cursor + 8..cursor + 16]
            .try_into()
            .map_err(|_| anyhow::anyhow!("Failed to read confidence bytes"))?;
        let confidence = f64::from_le_bytes(confidence_bytes);

        let timestamp_bytes: [u8; 8] = if data.len() >= cursor + 24 {
            data[cursor + 16..cursor + 24]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Failed to read timestamp bytes"))?
        } else {
            [0u8; 8]
        };
        let timestamp = i64::from_le_bytes(timestamp_bytes);

        if price.is_infinite() || price.is_nan() {
            return Err(anyhow::anyhow!("Invalid price value: {}", price));
        }
        
        if price == 0.0 {
            log::warn!("Switchboard oracle price is 0.0 - this might indicate parsing issue or empty oracle account");
        }

        Ok(PriceData {
            price,
            confidence: confidence.max(0.0),
            timestamp: if timestamp > 0 { timestamp } else { 
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| anyhow::anyhow!("Failed to get current time: {}", e))?
                    .as_secs() as i64
            },
        })
    }
}
