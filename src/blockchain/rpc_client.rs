use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient as SolanaRpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
    signature::Signature,
    hash::Hash,
};
use std::sync::Arc;
use tokio::time::{sleep, Duration};

pub struct RpcClient {
    client: Arc<SolanaRpcClient>,
    rpc_url: String,
}

impl RpcClient {
    pub fn new(rpc_url: String) -> Result<Self> {
        Ok(RpcClient {
            client: Arc::new(SolanaRpcClient::new_with_commitment(
                rpc_url.clone(),
                CommitmentConfig::confirmed(),
            )),
            rpc_url,
        })
    }

    pub async fn get_account(&self, pubkey: &Pubkey) -> Result<solana_sdk::account::Account> {
        let client = Arc::clone(&self.client);
        let pubkey = *pubkey;
        let rpc_url = self.rpc_url.clone();
        tokio::task::spawn_blocking(move || {
            client.get_account(&pubkey)
                .map_err(|e| anyhow::anyhow!("RPC error ({}): {}", rpc_url, e))
        })
        .await
        .context("Failed to spawn blocking task")?
    }

    pub async fn get_program_accounts(
        &self,
        program_id: &Pubkey,
    ) -> Result<Vec<(Pubkey, solana_sdk::account::Account)>> {
        let client = Arc::clone(&self.client);
        let program_id = *program_id;
        tokio::task::spawn_blocking(move || {
            client
                .get_program_accounts(&program_id)
                .map_err(|e| anyhow::anyhow!("RPC error: {}", e))
        })
        .await
        .context("Failed to spawn blocking task")?
    }

    pub async fn send_transaction(&self, tx: &solana_sdk::transaction::Transaction) -> Result<Signature> {
        let client = Arc::clone(&self.client);
        let tx = tx.clone();
        tokio::task::spawn_blocking(move || {
            client.send_transaction(&tx).map_err(|e| anyhow::anyhow!("RPC error: {}", e))
        })
        .await
        .context("Failed to spawn blocking task")?
    }

    pub async fn get_recent_blockhash(&self) -> Result<Hash> {
        let client = Arc::clone(&self.client);
        tokio::task::spawn_blocking(move || {
            client
                .get_latest_blockhash()
                .map_err(|e| anyhow::anyhow!("RPC error: {}", e))
        })
        .await
        .context("Failed to spawn blocking task")?
    }

    pub async fn get_slot(&self) -> Result<u64> {
        let client = Arc::clone(&self.client);
        tokio::task::spawn_blocking(move || {
            client
                .get_slot()
                .map_err(|e| anyhow::anyhow!("RPC error: {}", e))
        })
        .await
        .context("Failed to spawn blocking task")?
    }

    pub async fn get_multiple_accounts(
        &self,
        pubkeys: &[Pubkey],
    ) -> Result<Vec<Option<solana_sdk::account::Account>>> {
        let client = Arc::clone(&self.client);
        let pubkeys = pubkeys.to_vec();
        let rpc_url = self.rpc_url.clone();
        tokio::task::spawn_blocking(move || {
            client
                .get_multiple_accounts(&pubkeys)
                .map_err(|e| anyhow::anyhow!("RPC error ({}): {}", rpc_url, e))
        })
        .await
        .context("Failed to spawn blocking task")?
    }

    pub async fn simulate_transaction(
        &self,
        tx: &solana_sdk::transaction::Transaction,
    ) -> Result<solana_client::rpc_response::RpcSimulateTransactionResult> {
        let client = Arc::clone(&self.client);
        let tx = tx.clone();
        let rpc_url = self.rpc_url.clone();
        let response = tokio::task::spawn_blocking(move || {
            client.simulate_transaction(&tx)
        })
        .await
        .context("Failed to spawn blocking task")?
        .map_err(|e| anyhow::anyhow!("RPC error ({}): {}", rpc_url, e))?;
        Ok(response.value)
    }

    pub async fn retry<F, Fut, T>(&self, operation: F, max_retries: u32) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        for attempt in 0..=max_retries {
            match operation().await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    if attempt < max_retries {
                        let delay_ms = 1000 * (1 << attempt);
                        sleep(Duration::from_millis(delay_ms)).await;
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        unreachable!()
    }
}
