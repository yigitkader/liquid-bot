use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient as SolanaRpcClient;
use solana_client::rpc_config::RpcProgramAccountsConfig;
use solana_client::rpc_filter::RpcFilterType;
use solana_sdk::{
    commitment_config::CommitmentConfig, hash::Hash, pubkey::Pubkey, signature::Signature,
};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::time::{sleep, timeout, Duration, Instant};

struct TokenBucket {
    tokens: Arc<tokio::sync::Mutex<f64>>,
    capacity: f64,
    refill_rate: f64, // tokens per second
    last_refill: Arc<tokio::sync::Mutex<Instant>>,
}

impl TokenBucket {
    fn new(capacity: f64, refill_rate: f64) -> Self {
        TokenBucket {
            tokens: Arc::new(tokio::sync::Mutex::new(capacity)),
            capacity,
            refill_rate,
            last_refill: Arc::new(tokio::sync::Mutex::new(Instant::now())),
        }
    }

    async fn acquire(&self) {
        loop {
            let mut tokens = self.tokens.lock().await;
            let mut last_refill = self.last_refill.lock().await;

            let elapsed = last_refill.elapsed();
            let tokens_to_add = elapsed.as_secs_f64() * self.refill_rate;
            *tokens = (*tokens + tokens_to_add).min(self.capacity);
            *last_refill = Instant::now();

            if *tokens >= 1.0 {
                *tokens -= 1.0;
                return;
            }

            let tokens_needed = 1.0 - *tokens;
            let wait_time = Duration::from_secs_f64(tokens_needed / self.refill_rate);

            drop(tokens);
            drop(last_refill);

            sleep(wait_time).await;
        }
    }
}

pub struct RpcClient {
    client: Arc<SolanaRpcClient>,
    rpc_url: String,
    rate_limiter: Arc<Semaphore>,
    token_bucket: Arc<TokenBucket>,
    last_request_time: Arc<tokio::sync::Mutex<Instant>>,
    request_timeout: Duration,
}

impl RpcClient {
    pub fn new(rpc_url: String) -> Result<Self> {
        let request_timeout_seconds: u64 = std::env::var("RPC_TIMEOUT_SECONDS")
            .unwrap_or_else(|_| "10".to_string())
            .parse()
            .unwrap_or(10);

        let request_timeout = Duration::from_secs(request_timeout_seconds);

        if request_timeout_seconds > 30 {
            log::warn!("RPC_TIMEOUT_SECONDS={} is very high (>30s) - this may cause validation to block for too long", request_timeout_seconds);
        }

        log::info!(
            "RpcClient: Initialized with request_timeout={:?} (configure via RPC_TIMEOUT_SECONDS env var)",
            request_timeout
        );

        Ok(RpcClient {
            client: Arc::new(SolanaRpcClient::new_with_commitment(
                rpc_url.clone(),
                CommitmentConfig::confirmed(),
            )),
            rpc_url,
            rate_limiter: Arc::new(Semaphore::new(10)),
            token_bucket: Arc::new(TokenBucket::new(10.0, 10.0)),
            last_request_time: Arc::new(tokio::sync::Mutex::new(Instant::now())),
            request_timeout,
        })
    }

    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    async fn rate_limit(&self) {
        self.token_bucket.acquire().await;

        let mut last_time = self.last_request_time.lock().await;
        *last_time = Instant::now();
    }


    pub async fn get_account(&self, pubkey: &Pubkey) -> Result<solana_sdk::account::Account> {
        self.rate_limit().await;
        let _permit = self
            .rate_limiter
            .acquire()
            .await
            .context("Failed to acquire rate limiter permit")?;

        use crate::utils::error_helpers::{retry_with_backoff, RetryConfig};
        
        let pubkey = *pubkey;
        let client = Arc::clone(&self.client);
        let rpc_url = self.rpc_url.clone();
        let timeout_duration = self.request_timeout;
        
        retry_with_backoff(
            || {
                let client = Arc::clone(&client);
                let rpc_url = rpc_url.clone();
                let pubkey = pubkey;
                let timeout_duration = timeout_duration;
                async move {
                    timeout(
                        timeout_duration,
                        tokio::task::spawn_blocking(move || {
                            client
                                .get_account(&pubkey)
                                .map_err(|e| anyhow::anyhow!("RPC error ({}): {}", rpc_url, e))
                        }),
                    )
                    .await
                    .map_err(|_| anyhow::anyhow!(
                        "RPC request timeout after {:?} for get_account({})",
                        timeout_duration,
                        pubkey
                    ))?
                    .context("Failed to spawn blocking task")?
                    .map_err(|e| e)
                }
            },
            RetryConfig::for_rpc(),
        ).await
    }

    pub async fn get_program_accounts(
        &self,
        program_id: &Pubkey,
    ) -> Result<Vec<(Pubkey, solana_sdk::account::Account)>> {
        self.rate_limit().await;
        let _permit = self
            .rate_limiter
            .acquire()
            .await
            .context("Failed to acquire rate limiter permit")?;

        use crate::utils::error_helpers::{retry_with_backoff, RetryConfig};
        
        let program_id = *program_id;
        let client = Arc::clone(&self.client);
        let rpc_url = self.rpc_url.clone();
        let timeout_duration = (self.request_timeout * 3)
            .max(Duration::from_secs(30))
            .min(Duration::from_secs(90));
        
        retry_with_backoff(
            || {
                let client = Arc::clone(&client);
                let rpc_url = rpc_url.clone();
                let program_id = program_id;
                let timeout_duration = timeout_duration;
                async move {
                    timeout(
                        timeout_duration,
                        tokio::task::spawn_blocking(move || {
                            client
                                .get_program_accounts(&program_id)
                                .map_err(|e| anyhow::anyhow!("RPC error ({}): {}", rpc_url, e))
                        }),
                    )
                    .await
                    .map_err(|_| anyhow::anyhow!(
                        "RPC request timeout after {:?} for get_program_accounts({})",
                        timeout_duration,
                        program_id
                    ))?
                    .context("Failed to spawn blocking task")?
                    .map_err(|e| e)
                }
            },
            RetryConfig::for_rpc(),
        ).await
    }

    pub async fn get_program_accounts_with_filters(
        &self,
        program_id: &Pubkey,
        filters: Vec<RpcFilterType>,
    ) -> Result<Vec<(Pubkey, solana_sdk::account::Account)>> {
        self.rate_limit().await;
        let _permit = self
            .rate_limiter
            .acquire()
            .await
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

        // ✅ FIX: get_program_accounts_with_config is expensive and can take longer than default timeout
        // Use 3x the default timeout for this operation (min 30s, max 90s)
        let timeout_duration = self.request_timeout * 3;
        let timeout_duration = timeout_duration.max(Duration::from_secs(30)).min(Duration::from_secs(90));

        timeout(
            timeout_duration,
            tokio::task::spawn_blocking(move || {
                client
                    .get_program_accounts_with_config(&program_id, config)
                    .map_err(|e| anyhow::anyhow!("RPC error: {}", e))
            }),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "RPC request timeout after {:?} for get_program_accounts_with_config({})",
                timeout_duration,
                program_id
            )
        })?
        .context("Failed to spawn blocking task")?
    }

    pub async fn send_transaction(
        &self,
        tx: &solana_sdk::transaction::Transaction,
    ) -> Result<Signature> {
        self.rate_limit().await;
        let _permit = self
            .rate_limiter
            .acquire()
            .await
            .context("Failed to acquire rate limiter permit")?;

        let client = Arc::clone(&self.client);
        let tx = tx.clone();
        let timeout_duration = self.request_timeout;

        timeout(
            timeout_duration,
            tokio::task::spawn_blocking(move || {
                client
                    .send_transaction(&tx)
                    .map_err(|e| anyhow::anyhow!("RPC error: {}", e))
            }),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "RPC request timeout after {:?} for send_transaction",
                timeout_duration
            )
        })?
        .context("Failed to spawn blocking task")?
    }

    /// Check transaction signature status
    /// Returns Ok(Some(true)) if transaction is confirmed successfully
    /// Returns Ok(Some(false)) if transaction failed
    /// Returns Ok(None) if transaction not found (not yet confirmed or not in history)
    /// Returns Err if RPC error occurred
    pub async fn get_signature_status(&self, signature: &Signature) -> Result<Option<bool>> {
        self.rate_limit().await;
        let _permit = self
            .rate_limiter
            .acquire()
            .await
            .context("Failed to acquire rate limiter permit")?;

        let client = Arc::clone(&self.client);
        let sig = *signature;
        let timeout_duration = self.request_timeout;

        timeout(
            timeout_duration,
            tokio::task::spawn_blocking(move || {
                // Use get_signature_status (without config) - searches transaction history by default
                // Returns Result<Option<TransactionStatus>, Error>
                // TransactionStatus has an err field which is Option<TransactionError>
                match client.get_signature_status(&sig) {
                    Ok(Some(transaction_status)) => {
                        // transaction_status.err is Result<(), TransactionError>
                        if transaction_status.err().is_some() {
                            // Transaction failed
                            Ok(Some(false))
                        } else {
                            // Transaction confirmed (no error)
                            Ok(Some(true))
                        }
                    }
                    Ok(None) => {
                        // Transaction not found (not yet confirmed or not in history)
                        Ok(None)
                    }
                    Err(e) => Err(anyhow::anyhow!("RPC error checking signature status: {}", e)),
                }
            }),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "RPC request timeout after {:?} for get_signature_status",
                timeout_duration
            )
        })?
        .context("Failed to spawn blocking task")?
    }

    pub async fn get_recent_blockhash(&self) -> Result<Hash> {
        self.rate_limit().await;
        let _permit = self
            .rate_limiter
            .acquire()
            .await
            .context("Failed to acquire rate limiter permit")?;

        use crate::utils::error_helpers::{retry_with_backoff, RetryConfig};
        
        let client = Arc::clone(&self.client);
        let timeout_duration = self.request_timeout;
        
        retry_with_backoff(
            || {
                let client = Arc::clone(&client);
                let timeout_duration = timeout_duration;
                async move {
                    timeout(
                        timeout_duration,
                        tokio::task::spawn_blocking(move || {
                            client
                                .get_latest_blockhash()
                                .map_err(|e| anyhow::anyhow!("RPC error: {}", e))
                        }),
                    )
                    .await
                    .map_err(|_| anyhow::anyhow!(
                        "RPC request timeout after {:?} for get_latest_blockhash",
                        timeout_duration
                    ))?
                    .context("Failed to spawn blocking task")?
                    .map_err(|e| e)
                }
            },
            RetryConfig::for_rpc(),
        ).await
    }

    pub async fn get_slot(&self) -> Result<u64> {
        self.rate_limit().await;
        let _permit = self
            .rate_limiter
            .acquire()
            .await
            .context("Failed to acquire rate limiter permit")?;

        let client = Arc::clone(&self.client);
        let timeout_duration = self.request_timeout;

        timeout(
            timeout_duration,
            tokio::task::spawn_blocking(move || {
                client
                    .get_slot()
                    .map_err(|e| anyhow::anyhow!("RPC error: {}", e))
            }),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "RPC request timeout after {:?} for get_slot",
                timeout_duration
            )
        })?
        .context("Failed to spawn blocking task")?
    }

    pub async fn get_multiple_accounts(
        &self,
        pubkeys: &[Pubkey],
    ) -> Result<Vec<Option<solana_sdk::account::Account>>> {
        self.rate_limit().await;
        let _permit = self
            .rate_limiter
            .acquire()
            .await
            .context("Failed to acquire rate limiter permit")?;

        let client = Arc::clone(&self.client);
        let pubkeys_vec = pubkeys.to_vec();
        let pubkeys_count = pubkeys_vec.len(); // Save count before moving
        let rpc_url = self.rpc_url.clone();
        let timeout_duration = self.request_timeout;

        timeout(
            timeout_duration,
            tokio::task::spawn_blocking(move || {
                client
                    .get_multiple_accounts(&pubkeys_vec)
                    .map_err(|e| anyhow::anyhow!("RPC error ({}): {}", rpc_url, e))
            }),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "RPC request timeout after {:?} for get_multiple_accounts ({} accounts)",
                timeout_duration,
                pubkeys_count
            )
        })?
        .context("Failed to spawn blocking task")?
    }

    pub async fn simulate_transaction(
        &self,
        tx: &solana_sdk::transaction::Transaction,
    ) -> Result<solana_client::rpc_response::RpcSimulateTransactionResult> {
        self.rate_limit().await;
        let _permit = self
            .rate_limiter
            .acquire()
            .await
            .context("Failed to acquire rate limiter permit")?;

        let client = Arc::clone(&self.client);
        let tx = tx.clone();
        let rpc_url = self.rpc_url.clone();
        let timeout_duration = self.request_timeout;

        let response = timeout(
            timeout_duration,
            tokio::task::spawn_blocking(move || client.simulate_transaction(&tx)),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "RPC request timeout after {:?} for simulate_transaction",
                timeout_duration
            )
        })?
        .context("Failed to spawn blocking task")?
        .map_err(|e| anyhow::anyhow!("RPC error ({}): {}", rpc_url, e))?;
        Ok(response.value)
    }

}
