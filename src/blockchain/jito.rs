use anyhow::{Context, Result};
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
    system_instruction,
    hash::Hash,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use base64;

/// Jito Bundle for MEV protection
/// Bundles allow transactions to be included atomically in a block
#[derive(Debug, Clone)]
pub struct JitoBundle {
    transactions: Vec<Transaction>,
    tip_account: Pubkey,
    tip_amount: u64,
}

impl JitoBundle {
    pub fn new(tip_account: Pubkey, tip_amount: u64) -> Self {
        JitoBundle {
            transactions: Vec::new(),
            tip_account,
            tip_amount,
        }
    }

    pub fn add_transaction(&mut self, tx: Transaction) {
        self.transactions.push(tx);
    }

    pub fn transactions(&self) -> &[Transaction] {
        &self.transactions
    }
}

/// Jito client for sending bundles
pub struct JitoClient {
    url: String,
    tip_account: Pubkey,
    default_tip_amount: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct BundleResponse {
    #[serde(rename = "bundleId")]
    bundle_id: String,
    #[serde(rename = "slot")]
    slot: Option<u64>,
}

impl JitoClient {
    /// Create a new Jito client
    /// 
    /// # Arguments
    /// * `url` - Jito block engine URL (e.g., "https://mainnet.block-engine.jito.wtf")
    /// * `tip_account` - Jito tip account (e.g., "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZ6N6VBY6FuDgU3")
    /// * `default_tip_amount` - Default tip amount in lamports (e.g., 10_000_000 = 0.01 SOL)
    pub fn new(url: String, tip_account: Pubkey, default_tip_amount: u64) -> Self {
        JitoClient {
            url,
            tip_account,
            default_tip_amount,
        }
    }

    /// Send a bundle to Jito block engine
    /// 
    /// This method:
    /// 1. Serializes the bundle
    /// 2. Sends it to Jito block engine
    /// 3. Returns the bundle ID for tracking
    /// 
    /// Note: Tip transaction should be added separately using add_tip_transaction
    pub async fn send_bundle(&self, bundle: &JitoBundle) -> Result<String> {
        use reqwest::Client;

        // Serialize transactions to base64
        let serialized_txs: Vec<String> = bundle
            .transactions
            .iter()
            .map(|tx| {
                use bincode::serialize;
                let bytes = serialize(tx)
                    .map_err(|e| anyhow::anyhow!("Failed to serialize transaction: {}", e))?;
                Ok(base64::encode(&bytes))
            })
            .collect::<Result<Vec<_>>>()?;

        // Prepare bundle payload
        let payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendBundle",
            "params": [serialized_txs]
        });

        log::info!(
            "Jito: Sending bundle with {} transactions (tip: {} lamports)",
            bundle.transactions.len(),
            bundle.tip_amount
        );

        // Send to Jito block engine
        let client = Client::new();
        let response = client
            .post(&self.url)
            .json(&payload)
            .send()
            .await
            .context("Failed to send bundle to Jito")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Jito bundle send failed: status={}, body={}",
                status,
                text
            ));
        }

        let bundle_response: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse Jito response")?;

        if let Some(result) = bundle_response.get("result") {
            if let Some(bundle_id) = result.get("bundleId").and_then(|v| v.as_str()) {
                log::info!("✅ Jito bundle sent successfully: bundle_id={}", bundle_id);
                return Ok(bundle_id.to_string());
            }
        }

        if let Some(error) = bundle_response.get("error") {
            return Err(anyhow::anyhow!("Jito error: {}", error));
        }

        Err(anyhow::anyhow!("Unexpected Jito response format"))
    }

    /// Add a tip transaction to the bundle
    /// 
    /// The tip transaction pays the Jito tip account to incentivize
    /// block producers to include the bundle in the next block.
    pub fn add_tip_transaction(
        &self,
        bundle: &mut JitoBundle,
        wallet: &Arc<Keypair>,
        blockhash: Hash,
    ) -> Result<()> {
        let wallet_pubkey = wallet.pubkey();
        
        // Create transfer instruction to tip account
        let tip_ix = system_instruction::transfer(
            &wallet_pubkey,
            &bundle.tip_account,
            bundle.tip_amount,
        );
        
        // Build tip transaction
        let mut tip_tx = Transaction::new_with_payer(&[tip_ix], Some(&wallet_pubkey));
        tip_tx.message.recent_blockhash = blockhash;
        tip_tx.sign(&[wallet.as_ref()], blockhash);
        
        // Add tip transaction as the first transaction in the bundle
        // Tip must be first for Jito to process it correctly
        bundle.transactions.insert(0, tip_tx);
        
        log::debug!(
            "Jito: Added tip transaction: {} lamports to {}",
            bundle.tip_amount,
            bundle.tip_account
        );
        
        Ok(())
    }

    /// Get default tip account
    pub fn tip_account(&self) -> &Pubkey {
        &self.tip_account
    }

    /// Get default tip amount
    pub fn default_tip_amount(&self) -> u64 {
        self.default_tip_amount
    }
}

/// Helper function to create Jito client from config
pub fn create_jito_client(_config: &crate::core::config::Config) -> Option<JitoClient> {
    // Check if Jito is enabled in config
    // For now, we'll use default Jito mainnet settings
    let jito_url = std::env::var("JITO_BLOCK_ENGINE_URL")
        .unwrap_or_else(|_| "https://mainnet.block-engine.jito.wtf".to_string());
    
    let tip_account_str = std::env::var("JITO_TIP_ACCOUNT")
        .unwrap_or_else(|_| "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZ6N6VBY6FuDgU3".to_string());
    
    let tip_account = tip_account_str.parse::<Pubkey>().ok()?;
    let tip_amount = std::env::var("JITO_TIP_AMOUNT_LAMPORTS")
        .unwrap_or_else(|_| "10000000".to_string()) // 0.01 SOL
        .parse::<u64>()
        .ok()?;

    Some(JitoClient::new(jito_url, tip_account, tip_amount))
}

