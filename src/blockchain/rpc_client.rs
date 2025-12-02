use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient as SolanaRpcClient;
use solana_client::rpc_config::RpcProgramAccountsConfig;
use solana_client::rpc_filter::RpcFilterType;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
    signature::Signature,
    hash::Hash,
};
use std::sync::Arc;
use tokio::time::{sleep, Duration, Instant};
use tokio::sync::Semaphore;

pub struct RpcClient {
    client: Arc<SolanaRpcClient>,
    rpc_url: String,
    rate_limiter: Arc<Semaphore>,
    last_request_time: Arc<tokio::sync::Mutex<Instant>>,
}

impl RpcClient {
    pub fn new(rpc_url: String) -> Result<Self> {
        // Rate limiting: Max 10 concurrent requests, min 100ms between requests
        Ok(RpcClient {
            client: Arc::new(SolanaRpcClient::new_with_commitment(
                rpc_url.clone(),
                CommitmentConfig::confirmed(),
            )),
            rpc_url,
            rate_limiter: Arc::new(Semaphore::new(10)), // Max 10 concurrent requests
            last_request_time: Arc::new(tokio::sync::Mutex::new(Instant::now())),
        })
    }

    async fn rate_limit(&self) {
        // Minimum 100ms between requests to avoid rate limiting
        let mut last_time = self.last_request_time.lock().await;
        let elapsed = last_time.elapsed();
        if elapsed < Duration::from_millis(100) {
            sleep(Duration::from_millis(100) - elapsed).await;
        }
        *last_time = Instant::now();
    }

    async fn should_retry_rate_limit(&self, error_str: &str, retry_count: &mut u32) -> bool {
        if error_str.contains("429") || error_str.contains("Too many requests") || error_str.contains("rate limit") {
            *retry_count += 1;
            if *retry_count <= 5 {
                let backoff = Duration::from_millis(500 * (*retry_count as u64));
                log::warn!("⚠️  Rate limit hit (429), backing off for {:?} (attempt {}/{})", backoff, *retry_count, 5);
                sleep(backoff).await;
                return true; // Retry
            } else {
                log::error!("❌ Rate limit exceeded after 5 retries");
                return false; // Don't retry
            }
        }
        false
    }

    pub async fn get_account(&self, pubkey: &Pubkey) -> Result<solana_sdk::account::Account> {
        self.rate_limit().await;
        let _permit = self.rate_limiter.acquire().await
            .context("Failed to acquire rate limiter permit")?;
        
        let pubkey = *pubkey;
        let mut retry_count = 0;
        
        loop {
            let client = Arc::clone(&self.client);
            let rpc_url = self.rpc_url.clone();
            
            let result = tokio::task::spawn_blocking(move || {
                client.get_account(&pubkey)
                    .map_err(|e| anyhow::anyhow!("RPC error ({}): {}", rpc_url, e))
            })
            .await
            .context("Failed to spawn blocking task");
            
            match result {
                Ok(Ok(account)) => return Ok(account),
                Ok(Err(e)) => {
                    let error_str = e.to_string();
                    if !self.should_retry_rate_limit(&error_str, &mut retry_count).await {
                        return Err(e);
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub async fn get_program_accounts(
        &self,
        program_id: &Pubkey,
    ) -> Result<Vec<(Pubkey, solana_sdk::account::Account)>> {
        self.rate_limit().await;
        let _permit = self.rate_limiter.acquire().await
            .context("Failed to acquire rate limiter permit")?;
        
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

    /// Get program accounts with filters to reduce data transfer and processing.
    /// This is much more efficient than fetching all accounts and filtering client-side.
    pub async fn get_program_accounts_with_filters(
        &self,
        program_id: &Pubkey,
        filters: Vec<RpcFilterType>,
    ) -> Result<Vec<(Pubkey, solana_sdk::account::Account)>> {
        self.rate_limit().await;
        let _permit = self.rate_limiter.acquire().await
            .context("Failed to acquire rate limiter permit")?;
        
        let client = Arc::clone(&self.client);
        let program_id = *program_id;
        
        let config = RpcProgramAccountsConfig {
            filters: Some(filters),
            account_config: solana_client::rpc_config::RpcAccountInfoConfig {
                encoding: Some(solana_account_decoder::UiAccountEncoding::Base64),
                commitment: Some(CommitmentConfig::confirmed()),
                ..Default::default()
            },
            ..Default::default()
        };
        
        tokio::task::spawn_blocking(move || {
            client
                .get_program_accounts_with_config(&program_id, config)
                .map_err(|e| anyhow::anyhow!("RPC error: {}", e))
        })
        .await
        .context("Failed to spawn blocking task")?
    }

    pub async fn send_transaction(&self, tx: &solana_sdk::transaction::Transaction) -> Result<Signature> {
        self.rate_limit().await;
        let _permit = self.rate_limiter.acquire().await
            .context("Failed to acquire rate limiter permit")?;
        
        let client = Arc::clone(&self.client);
        let tx = tx.clone();
        tokio::task::spawn_blocking(move || {
            client.send_transaction(&tx).map_err(|e| anyhow::anyhow!("RPC error: {}", e))
        })
        .await
        .context("Failed to spawn blocking task")?
    }

    pub async fn get_recent_blockhash(&self) -> Result<Hash> {
        self.rate_limit().await;
        let _permit = self.rate_limiter.acquire().await
            .context("Failed to acquire rate limiter permit")?;
        
        let mut retry_count = 0;
        
        loop {
            let client = Arc::clone(&self.client);
            
            let result = tokio::task::spawn_blocking(move || {
                client
                    .get_latest_blockhash()
                    .map_err(|e| anyhow::anyhow!("RPC error: {}", e))
            })
            .await
            .context("Failed to spawn blocking task");
            
            match result {
                Ok(Ok(hash)) => return Ok(hash),
                Ok(Err(e)) => {
                    let error_str = e.to_string();
                    if !self.should_retry_rate_limit(&error_str, &mut retry_count).await {
                        return Err(e);
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub async fn get_slot(&self) -> Result<u64> {
        self.rate_limit().await;
        let _permit = self.rate_limiter.acquire().await
            .context("Failed to acquire rate limiter permit")?;
        
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
        self.rate_limit().await;
        let _permit = self.rate_limiter.acquire().await
            .context("Failed to acquire rate limiter permit")?;
        
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
        self.rate_limit().await;
        let _permit = self.rate_limiter.acquire().await
            .context("Failed to acquire rate limiter permit")?;
        
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
