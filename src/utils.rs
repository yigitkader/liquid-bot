use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    hash::Hash,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
#[allow(deprecated)]
use solana_sdk::system_instruction;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

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
    pub fn new(url: String, tip_account: Pubkey, default_tip_amount: u64) -> Self {
        JitoClient {
            url,
            tip_account,
            default_tip_amount,
        }
    }

    /// Get tip accounts dynamically from Jito Block Engine API
    /// This is the recommended way to get tip accounts - they can change over time
    /// Returns a list of tip account addresses that are currently active
    pub async fn get_tip_accounts(&self) -> Result<Vec<Pubkey>> {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getTipAccounts",
            "params": []
        });

        log::debug!("Fetching tip accounts from Jito Block Engine...");

        let client = reqwest::Client::new();
        let response = client
            .post(&self.url)
            .json(&payload)
            .send()
            .await
            .context("Failed to request tip accounts from Jito")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Jito getTipAccounts failed: status={}, body={}",
                status,
                text
            ));
        }

        let response_json: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse Jito getTipAccounts response")?;

        // Parse response: {"result": ["account1", "account2", ...]}
        if let Some(result) = response_json.get("result") {
            if let Some(accounts_array) = result.as_array() {
                let mut tip_accounts = Vec::new();
                for account_str in accounts_array {
                    if let Some(addr) = account_str.as_str() {
                        match Pubkey::from_str(addr) {
                            Ok(pubkey) => tip_accounts.push(pubkey),
                            Err(e) => {
                                log::warn!("Invalid tip account address from Jito: {} ({})", addr, e);
                            }
                        }
                    }
                }
                if !tip_accounts.is_empty() {
                    log::info!("✅ Fetched {} tip accounts from Jito Block Engine", tip_accounts.len());
                    return Ok(tip_accounts);
                }
            }
        }

        // If error in response
        if let Some(error) = response_json.get("error") {
            return Err(anyhow::anyhow!("Jito getTipAccounts error: {}", error));
        }

        Err(anyhow::anyhow!("Unexpected Jito getTipAccounts response format"))
    }

    pub async fn send_bundle(&self, bundle: &JitoBundle) -> Result<String> {
        let serialized_txs: Vec<String> = bundle
            .transactions
            .iter()
            .map(|tx| {
                let bytes = bincode::serialize(tx)
                    .map_err(|e| anyhow::anyhow!("Failed to serialize transaction: {}", e))?;
                use base64::Engine;
                Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
            })
            .collect::<Result<Vec<_>, anyhow::Error>>()?;

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

        let client = reqwest::Client::new();
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
            if let Ok(bundle_resp) = serde_json::from_value::<BundleResponse>(result.clone()) {
                log::info!(
                    "✅ Jito bundle sent successfully: bundle_id={}, slot={:?}",
                    bundle_resp.bundle_id,
                    bundle_resp.slot
                );
                return Ok(bundle_resp.bundle_id);
            }

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

    pub fn add_tip_transaction(
        &self,
        bundle: &mut JitoBundle,
        wallet: &Arc<Keypair>,
        blockhash: Hash,
    ) -> Result<()> {
        let wallet_pubkey = wallet.pubkey();

        let tip_ix =
            system_instruction::transfer(&wallet_pubkey, &bundle.tip_account, bundle.tip_amount);

        let mut tip_tx = Transaction::new_with_payer(&[tip_ix], Some(&wallet_pubkey));
        tip_tx.message.recent_blockhash = blockhash;

        sign_transaction(&mut tip_tx, wallet.as_ref())
            .context("Failed to sign Jito tip transaction")?;

        bundle.transactions.push(tip_tx);

        log::debug!(
            "Jito: Appended tip transaction: {} lamports to {}",
            bundle.tip_amount,
            bundle.tip_account
        );

        Ok(())
    }

    pub fn tip_account(&self) -> &Pubkey {
        &self.tip_account
    }

    pub fn default_tip_amount(&self) -> u64 {
        self.default_tip_amount
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Get bundle status from Jito API
    /// Returns the status of a bundle (confirmed, pending, failed, etc.)
    pub async fn get_bundle_status(&self, bundle_id: &str) -> Result<Option<BundleStatusResponse>> {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getBundleStatuses",
            "params": [[bundle_id]]
        });

        let client = reqwest::Client::new();
        let response = client
            .post(&self.url)
            .json(&payload)
            .send()
            .await
            .context("Failed to query bundle status from Jito")?;

        if !response.status().is_success() {
            log::debug!("Jito bundle status query failed: status={}", response.status());
            return Ok(None);
        }

        let bundle_response: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse Jito bundle status response")?;

        if let Some(result) = bundle_response.get("result") {
            if let Some(statuses) = result.as_array() {
                if let Some(first_status) = statuses.get(0) {
                    if let Ok(status) = serde_json::from_value::<BundleStatusResponse>(first_status.clone()) {
                        return Ok(Some(status));
                    }
                }
            }
        }

        Ok(None)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleStatusResponse {
    #[serde(rename = "bundleId")]
    pub bundle_id: String,
    pub status: Option<String>, // "landed", "failed", "pending", etc.
    pub slot: Option<u64>,
}

/// Sign a transaction with a keypair
pub fn sign_transaction(tx: &mut Transaction, keypair: &Keypair) -> Result<()> {
    let signer_pubkey = keypair.pubkey();

    let signer_index = tx
        .message
        .account_keys
        .iter()
        .position(|&pk| pk == signer_pubkey)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Transaction signing failed: signer {} not found in account_keys",
                signer_pubkey
            )
        })?;

    let num_required_signatures = tx.message.header.num_required_signatures as usize;
    if tx.signatures.len() != num_required_signatures {
        tx.signatures
            .resize(num_required_signatures, solana_sdk::signature::Signature::default());
    }

    if signer_index >= num_required_signatures {
        return Err(anyhow::anyhow!(
            "Transaction signing failed: signer {} at index {} is not in required signatures range (0..{})",
            signer_pubkey,
            signer_index,
            num_required_signatures
        ));
    }

    if signer_index < tx.signatures.len() {
        let existing_sig = &tx.signatures[signer_index];
        if *existing_sig != solana_sdk::signature::Signature::default() {
            log::debug!(
                "Transaction already signed at index {} - skipping re-signing",
                signer_index
            );
            return Ok(());
        }
    }

    let message_bytes = tx.message.serialize();
    let signature = keypair.sign_message(&message_bytes);

    if signer_index >= tx.signatures.len() {
        return Err(anyhow::anyhow!(
            "Transaction signing failed: signer index {} out of bounds (signatures.len()={})",
            signer_index,
            tx.signatures.len()
        ));
    }
    tx.signatures[signer_index] = signature;

    Ok(())
}

/// Send Jito bundle with tip transaction
pub async fn send_jito_bundle(
    mut tx: Transaction,
    jito_client: &JitoClient,
    wallet: &Arc<Keypair>,
    blockhash: Hash,
) -> Result<String> {
    let mut bundle = JitoBundle::new(
        *jito_client.tip_account(),
        jito_client.default_tip_amount(),
    );

    // Sign main transaction first
    sign_transaction(&mut tx, wallet.as_ref())
        .context("Failed to sign main transaction")?;

    // Add main transaction to bundle (FIRST)
    bundle.add_transaction(tx);

    // Add tip transaction to bundle (LAST)
    // CRITICAL: Tip transaction should be the last instruction in the bundle
    // This ensures that if the main transaction fails (e.g. simulation error),
    // the tip is NOT paid (since the whole bundle fails atomically).
    // However, for Jito specifically, bundles are atomic "all or nothing", so order
    // matters less for atomicity but matters for sequential dependency.
    // Placing tip last is the standard practice.
    jito_client
        .add_tip_transaction(&mut bundle, wallet, blockhash)
        .context("Failed to add tip transaction")?;

    jito_client
        .send_bundle(&bundle)
        .await
        .context("Failed to send Jito bundle")
}

/// RPC call with retry mechanism
/// Handles transient network errors with exponential backoff
/// 
/// NOTE: This is a wrapper that can be used for any RPC call that returns Result<T, ClientError>
/// For now, we provide a simple boolean check version
pub async fn rpc_account_exists_with_retry(
    rpc: &Arc<RpcClient>,
    pubkey: &Pubkey,
    max_retries: u32,
) -> bool {
    
    for attempt in 1..=max_retries {
        match rpc.get_account(pubkey) {
            Ok(_) => {
                if attempt > 1 {
                    log::debug!("RPC get_account succeeded on attempt {}/{}", attempt, max_retries);
                }
                return true;
            }
            Err(_) => {
                if attempt < max_retries {
                    // ✅ FIXED: Exponential backoff with jitter
                    // Base delay: 200ms * attempt (200ms, 400ms, 600ms...)
                    // Jitter: random 0-100ms to prevent thundering herd
                    use rand::Rng;
                    let base_delay_ms = 200 * attempt as u64;
                    let jitter_ms = rand::thread_rng().gen_range(0..100);
                    let delay_ms = base_delay_ms + jitter_ms;
                    log::debug!(
                        "RPC get_account attempt {}/{} failed for {}, retrying in {}ms (base: {}ms + jitter: {}ms)...",
                        attempt,
                        max_retries,
                        pubkey,
                        delay_ms,
                        base_delay_ms,
                        jitter_ms
                    );
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }
            }
        }
    }
    
    false
}

