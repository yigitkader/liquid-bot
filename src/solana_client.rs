use crate::config::Config;
use crate::domain::LiquidationOpportunity;
use crate::protocol::Protocol;
use crate::rate_limiter::RateLimiter;
use crate::wallet::WalletManager;
use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig, compute_budget::ComputeBudgetInstruction, pubkey::Pubkey,
    transaction::Transaction,
};
use std::sync::Arc;

/// Solana RPC client wrapper
pub struct SolanaClient {
    rpc_client: Arc<RpcClient>,
    rpc_url: String, // RPC URL'yi sakla (Arc için gerekli)
    commitment: CommitmentConfig,
    rate_limiter: Arc<RateLimiter>,
}

impl SolanaClient {
    pub fn new(rpc_url: String) -> Result<Self> {
        let rpc_client = Arc::new(RpcClient::new_with_commitment(
            rpc_url.clone(),
            CommitmentConfig::confirmed(), // Confirmed commitment level
        ));

        let rate_limiter = Arc::new(RateLimiter::new(100));

        Ok(SolanaClient {
            rpc_client,
            rpc_url,
            commitment: CommitmentConfig::confirmed(),
            rate_limiter,
        })
    }

    /// RPC URL'yi döndürür
    pub fn get_url(&self) -> &str {
        &self.rpc_url
    }

    pub async fn get_account(&self, pubkey: &Pubkey) -> Result<solana_sdk::account::Account> {
        self.rate_limiter.wait_if_needed().await;

        // todo RpcClient sync, ama async wrapper kullanabiliriz
        let client = Arc::clone(&self.rpc_client);
        let pubkey = *pubkey;
        tokio::task::spawn_blocking(move || {
            client
                .get_account(&pubkey)
                .map_err(|e| anyhow::anyhow!("RPC error: {}", e))
        })
        .await
        .context("Failed to spawn blocking task")?
    }

    pub async fn get_program_accounts(
        &self,
        program_id: &Pubkey,
    ) -> Result<Vec<(Pubkey, solana_sdk::account::Account)>> {
        self.rate_limiter.wait_if_needed().await;

        let client = Arc::clone(&self.rpc_client);
        let program_id = *program_id;
        tokio::task::spawn_blocking(move || {
            client
                .get_program_accounts(&program_id)
                .map_err(|e| anyhow::anyhow!("RPC error: {}", e))
        })
        .await
        .context("Failed to spawn blocking task")?
    }

    pub async fn send_transaction(&self, transaction: &Transaction) -> Result<String> {
        self.rate_limiter.wait_if_needed().await;

        let client = Arc::clone(&self.rpc_client);
        let tx = transaction.clone();
        let signature = tokio::task::spawn_blocking(move || {
            client
                .send_transaction(&tx)
                .map_err(|e| anyhow::anyhow!("Failed to send transaction: {}", e))
        })
        .await
        .context("Failed to spawn blocking task")??;

        Ok(signature.to_string())
    }

    pub async fn get_recent_blockhash(&self) -> Result<solana_sdk::hash::Hash> {
        // Rate limiting
        self.rate_limiter.wait_if_needed().await;

        let client = Arc::clone(&self.rpc_client);
        tokio::task::spawn_blocking(move || {
            client
                .get_latest_blockhash()
                .map_err(|e| anyhow::anyhow!("Failed to get recent blockhash: {}", e))
        })
        .await
        .context("Failed to spawn blocking task")?
    }

    pub async fn get_slot(&self) -> Result<u64> {
        // Rate limiting
        self.rate_limiter.wait_if_needed().await;

        let client = Arc::clone(&self.rpc_client);
        tokio::task::spawn_blocking(move || {
            client
                .get_slot()
                .map_err(|e| anyhow::anyhow!("Failed to get slot: {}", e))
        })
        .await
        .context("Failed to spawn blocking task")?
    }
}

pub async fn execute_liquidation(
    opportunity: &LiquidationOpportunity,
    _config: &Config,
    wallet: &WalletManager,
    protocol: &dyn Protocol,
    rpc_client: Arc<SolanaClient>,
) -> Result<String> {
    log::info!(
        "Executing liquidation for account: {}",
        opportunity.account_position.account_address
    );

    let recent_blockhash = rpc_client.get_recent_blockhash().await?;
    let liquidator_pubkey = wallet.pubkey();
    let liquidation_ix = protocol
        .build_liquidation_instruction(
            opportunity,
            liquidator_pubkey,
            Some(Arc::clone(&rpc_client)),
        )
        .await
        .context("Failed to build liquidation instruction")?;

    let compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(200_000);
    let priority_fee_ix = ComputeBudgetInstruction::set_compute_unit_price(1_000);

    let instructions = vec![compute_budget_ix, priority_fee_ix, liquidation_ix];

    let mut transaction = Transaction::new_with_payer(&instructions, Some(liquidator_pubkey));

    transaction.message.recent_blockhash = recent_blockhash;

    transaction.sign(&[wallet.keypair()], recent_blockhash);

    let signature = rpc_client.send_transaction(&transaction).await?;

    log::info!("Liquidation transaction sent: {}", signature);

    Ok(signature)
}
