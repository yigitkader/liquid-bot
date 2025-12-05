use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use solana_sdk::{
    hash::Hash,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction,
    transaction::Transaction,
};
use std::sync::Arc;

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

        bundle.transactions.insert(0, tip_tx);

        log::debug!(
            "Jito: Added tip transaction: {} lamports to {}",
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

    jito_client
        .add_tip_transaction(&mut bundle, wallet, blockhash)
        .context("Failed to add tip transaction")?;

    sign_transaction(&mut tx, wallet.as_ref())
        .context("Failed to sign main transaction")?;

    bundle.add_transaction(tx);

    jito_client
        .send_bundle(&bundle)
        .await
        .context("Failed to send Jito bundle")
}

