use crate::config::Config;
use crate::domain::LiquidationOpportunity;
use crate::protocol::Protocol;
use crate::rate_limiter::RateLimiter;
use crate::wallet::WalletManager;
use anyhow::{Context, Result};
use rand::Rng;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig, compute_budget::ComputeBudgetInstruction, pubkey::Pubkey,
    transaction::Transaction,
};
use std::sync::Arc;
use tokio::time::{sleep, Duration};

pub struct SolanaClient {
    rpc_client: Arc<RpcClient>,
    rpc_url: String,
    #[allow(dead_code)] // Reserved for future use when we need to query with different commitment levels
    commitment: CommitmentConfig,
    rate_limiter: Arc<RateLimiter>,
}

impl SolanaClient {
    pub fn new(rpc_url: String) -> Result<Self> {
        Ok(SolanaClient {
            rpc_client: Arc::new(RpcClient::new_with_commitment(
                rpc_url.clone(),
                CommitmentConfig::confirmed(),
            )),
            rpc_url,
            commitment: CommitmentConfig::confirmed(),
            rate_limiter: Arc::new(RateLimiter::new(100)),
        })
    }

    pub fn get_url(&self) -> &str {
        &self.rpc_url
    }

    pub async fn get_account(&self, pubkey: &Pubkey) -> Result<solana_sdk::account::Account> {
        self.rate_limiter.wait_if_needed().await;
        let client = Arc::clone(&self.rpc_client);
        let pubkey = *pubkey;
        tokio::task::spawn_blocking(move || {
            client.get_account(&pubkey).map_err(|e| anyhow::anyhow!("RPC error: {}", e))
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

        Self::fetch_with_retry(
            {
                let client = Arc::clone(&client);
                let program_id = program_id;
                move || {
                    let client = Arc::clone(&client);
                    let program_id = program_id;
                    async move {
                        tokio::task::spawn_blocking(move || {
                            client
                                .get_program_accounts(&program_id)
                                .map_err(|e| anyhow::anyhow!("RPC error: {}", e))
                        })
                        .await
                        .context("Failed to spawn blocking task")?
                    }
                }
            },
            5,
            1000,
        )
        .await
    }

    async fn fetch_with_retry<T, F, Fut>(operation: F, max_retries: u32, jitter_max_ms: u64) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        for attempt in 0..=max_retries {
            match operation().await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    let error_str = e.to_string().to_lowercase();
                    let is_rate_limit = error_str.contains("429")
                        || error_str.contains("rate limit")
                        || error_str.contains("too many requests")
                        || error_str.contains("rate_limit");

                    if is_rate_limit && attempt < max_retries {
                        let base_delay_secs = 2_u64.pow(attempt);
                        let jitter_ms = rand::thread_rng().gen_range(0..jitter_max_ms);
                        let backoff = Duration::from_secs(base_delay_secs) + Duration::from_millis(jitter_ms);

                        log::warn!(
                            "Rate limit error (attempt {}/{}), backing off for {:.2}s",
                            attempt + 1,
                            max_retries + 1,
                            backoff.as_secs_f64()
                        );

                        sleep(backoff).await;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
        unreachable!()
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
    config: &Config,
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

    let compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(config.default_compute_units);
    let priority_fee_ix = ComputeBudgetInstruction::set_compute_unit_price(config.default_priority_fee_per_cu);

    let instructions = vec![compute_budget_ix, priority_fee_ix, liquidation_ix];

    let mut transaction = Transaction::new_with_payer(&instructions, Some(liquidator_pubkey));

    transaction.message.recent_blockhash = recent_blockhash;

    transaction.sign(&[wallet.keypair()], recent_blockhash);

    let signature = rpc_client.send_transaction(&transaction).await?;

    log::info!("Liquidation transaction sent: {}", signature);

    Ok(signature)
}
