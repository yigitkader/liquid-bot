use crate::blockchain::rpc_client::RpcClient;
use anyhow::Result;
use pyth_sdk_solana::state::SolanaPriceAccount;
use solana_sdk::pubkey::Pubkey;
use solana_account::Account as SolanaAccount;
use solana_pubkey::Pubkey as SolanaPubkey;
use std::sync::Arc;

pub struct PythOracle;

#[derive(Debug, Clone)]
pub struct PriceData {
    pub price: f64,
    pub confidence: f64,
    pub expo: i32,
    pub timestamp: i64,
}

impl PythOracle {
    pub async fn read_price(
        account: &Pubkey,
        rpc: Arc<RpcClient>,
        config: Option<&crate::config::Config>,
    ) -> Result<PriceData> {
        let account_data = rpc.get_account(account).await?;
        Self::parse_pyth_account(&account_data.data, config)
    }

    pub fn parse_pyth_account(
        data: &[u8],
        config: Option<&crate::config::Config>,
    ) -> Result<PriceData> {
        let pyth_program_id_str = config
            .map(|c| c.pyth_program_id.as_str())
            .unwrap_or("FsJ3A3u2vn5cTVofAjvy6y5kwABJAqYWpe4975bi2epH");
        let pyth_program_id = pyth_program_id_str
            .parse::<Pubkey>()
            .map_err(|e| anyhow::anyhow!("Invalid Pyth program ID: {}", e))?;

        // Convert solana_sdk types to solana_account/solana_pubkey types for Pyth SDK
        // Both Pubkey types are [u8; 32], so we can convert via bytes
        let pyth_program_id_bytes: [u8; 32] = pyth_program_id.to_bytes();
        let pyth_program_id_solana = SolanaPubkey::from(pyth_program_id_bytes);

        let mut account = SolanaAccount {
            lamports: 0,
            data: data.to_vec(),
            owner: pyth_program_id_solana,
            executable: false,
            rent_epoch: 0,
        };

        let feed = SolanaPriceAccount::account_to_feed(&pyth_program_id_solana, &mut account)
            .map_err(|e| anyhow::anyhow!("Failed to parse Pyth account: {}", e))?;

        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| anyhow::anyhow!("Failed to get current time: {}", e))?
            .as_secs() as i64;

        let price_data = feed
            .get_price_no_older_than(current_time, 60)
            .ok_or_else(|| anyhow::anyhow!("Price data is too old"))?;

        Ok(PriceData {
            price: price_data.price as f64 * 10_f64.powi(price_data.expo),
            confidence: price_data.conf as f64 * 10_f64.powi(price_data.expo),
            expo: price_data.expo,
            timestamp: price_data.publish_time,
        })
    }
}
