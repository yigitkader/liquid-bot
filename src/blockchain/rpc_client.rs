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
use tokio::time::{sleep, Duration, Instant, timeout};
use tokio::sync::Semaphore;

/// Token bucket for rate limiting with burst support
/// Allows burst requests up to capacity, then refills at refill_rate tokens/second
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

    /// Acquire a token, waiting if necessary
    async fn acquire(&self) {
        loop {
            let mut tokens = self.tokens.lock().await;
            let mut last_refill = self.last_refill.lock().await;
            
            // Refill tokens based on elapsed time
            let elapsed = last_refill.elapsed();
            let tokens_to_add = elapsed.as_secs_f64() * self.refill_rate;
            *tokens = (*tokens + tokens_to_add).min(self.capacity);
            *last_refill = Instant::now();
            
            // If we have at least 1 token, consume it and return
            if *tokens >= 1.0 {
                *tokens -= 1.0;
                return;
            }
            
            // Otherwise, calculate how long to wait
            let tokens_needed = 1.0 - *tokens;
            let wait_time = Duration::from_secs_f64(tokens_needed / self.refill_rate);
            
            // Drop locks before sleeping
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
    /// RPC request timeout - prevents requests from hanging indefinitely
    /// Default: 10 seconds (reasonable for most RPC calls)
    /// Can be configured via RPC_TIMEOUT_SECONDS env var
    request_timeout: Duration,
}

impl RpcClient {
    pub fn new(rpc_url: String) -> Result<Self> {
        // ✅ CRITICAL: Configure RPC timeout to prevent requests from hanging
        // Default: 10 seconds (reasonable for most RPC calls)
        // Can be overridden via RPC_TIMEOUT_SECONDS env var
        let request_timeout_seconds: u64 = std::env::var("RPC_TIMEOUT_SECONDS")
            .unwrap_or_else(|_| "10".to_string())
            .parse()
            .unwrap_or(10); // Fallback to 10s if parse fails
        
        let request_timeout = Duration::from_secs(request_timeout_seconds);
        
        if request_timeout_seconds > 30 {
            log::warn!(
                "⚠️  RPC_TIMEOUT_SECONDS={} is very high (>30s) - this may cause validation to block for too long",
                request_timeout_seconds
            );
        }
        
        log::info!(
            "RpcClient: Initialized with request_timeout={:?} (configure via RPC_TIMEOUT_SECONDS env var)",
            request_timeout
        );
        
        // Rate limiting configuration:
        // - Semaphore: Max 10 concurrent requests
        // - Token bucket: Capacity 10 tokens, refill rate 10 tokens/second (allows 10 req/s sustained, burst up to 10)
        // - Last request time: Legacy fixed delay (kept for backward compatibility)
        Ok(RpcClient {
            client: Arc::new(SolanaRpcClient::new_with_commitment(
                rpc_url.clone(),
                CommitmentConfig::confirmed(),
            )),
            rpc_url,
            rate_limiter: Arc::new(Semaphore::new(10)), // Max 10 concurrent requests
            token_bucket: Arc::new(TokenBucket::new(10.0, 10.0)), // 10 tokens capacity, 10 tokens/second refill
            last_request_time: Arc::new(tokio::sync::Mutex::new(Instant::now())),
            request_timeout,
        })
    }

    /// Get the RPC request timeout duration
    /// This is useful for coordinating timeouts in other components (e.g., oracle reads)
    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    async fn rate_limit(&self) {
        // Use token bucket for rate limiting (handles burst requests better)
        self.token_bucket.acquire().await;
        
        // Legacy: Update last request time (kept for compatibility, but token bucket is primary)
        let mut last_time = self.last_request_time.lock().await;
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
            
            // ✅ CRITICAL: Add timeout to prevent RPC calls from hanging indefinitely
            // This ensures validation doesn't block for too long
            let timeout_duration = self.request_timeout;
            let result = timeout(
                timeout_duration,
                tokio::task::spawn_blocking(move || {
                    client.get_account(&pubkey)
                        .map_err(|e| anyhow::anyhow!("RPC error ({}): {}", rpc_url, e))
                })
            )
            .await;
            
            let result = match result {
                Ok(task_result) => task_result.context("Failed to spawn blocking task"),
                Err(_) => {
                    // Timeout occurred
                    return Err(anyhow::anyhow!(
                        "RPC request timeout after {:?} for get_account({})",
                        timeout_duration,
                        pubkey
                    ));
                }
            };
            
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
        let timeout_duration = self.request_timeout;
        
        timeout(
            timeout_duration,
            tokio::task::spawn_blocking(move || {
                client
                    .get_program_accounts(&program_id)
                    .map_err(|e| anyhow::anyhow!("RPC error: {}", e))
            })
        )
        .await
        .map_err(|_| anyhow::anyhow!(
            "RPC request timeout after {:?} for get_program_accounts({})",
            timeout_duration,
            program_id
        ))?
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
        
        let timeout_duration = self.request_timeout;
        
        timeout(
            timeout_duration,
            tokio::task::spawn_blocking(move || {
                client
                    .get_program_accounts_with_config(&program_id, config)
                    .map_err(|e| anyhow::anyhow!("RPC error: {}", e))
            })
        )
        .await
        .map_err(|_| anyhow::anyhow!(
            "RPC request timeout after {:?} for get_program_accounts_with_config({})",
            timeout_duration,
            program_id
        ))?
        .context("Failed to spawn blocking task")?
    }

    pub async fn send_transaction(&self, tx: &solana_sdk::transaction::Transaction) -> Result<Signature> {
        self.rate_limit().await;
        let _permit = self.rate_limiter.acquire().await
            .context("Failed to acquire rate limiter permit")?;
        
        let client = Arc::clone(&self.client);
        let tx = tx.clone();
        let timeout_duration = self.request_timeout;
        
        timeout(
            timeout_duration,
            tokio::task::spawn_blocking(move || {
                client.send_transaction(&tx).map_err(|e| anyhow::anyhow!("RPC error: {}", e))
            })
        )
        .await
        .map_err(|_| anyhow::anyhow!(
            "RPC request timeout after {:?} for send_transaction",
            timeout_duration
        ))?
        .context("Failed to spawn blocking task")?
    }

    pub async fn get_recent_blockhash(&self) -> Result<Hash> {
        self.rate_limit().await;
        let _permit = self.rate_limiter.acquire().await
            .context("Failed to acquire rate limiter permit")?;
        
        let mut retry_count = 0;
        
        loop {
            let client = Arc::clone(&self.client);
            
            let timeout_duration = self.request_timeout;
            let result = timeout(
                timeout_duration,
                tokio::task::spawn_blocking(move || {
                    client
                        .get_latest_blockhash()
                        .map_err(|e| anyhow::anyhow!("RPC error: {}", e))
                })
            )
            .await;
            
            let result = match result {
                Ok(task_result) => task_result.context("Failed to spawn blocking task"),
                Err(_) => {
                    // Timeout occurred
                    return Err(anyhow::anyhow!(
                        "RPC request timeout after {:?} for get_latest_blockhash",
                        timeout_duration
                    ));
                }
            };
            
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
        let timeout_duration = self.request_timeout;
        
        timeout(
            timeout_duration,
            tokio::task::spawn_blocking(move || {
                client
                    .get_slot()
                    .map_err(|e| anyhow::anyhow!("RPC error: {}", e))
            })
        )
        .await
        .map_err(|_| anyhow::anyhow!(
            "RPC request timeout after {:?} for get_slot",
            timeout_duration
        ))?
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
            })
        )
        .await
        .map_err(|_| anyhow::anyhow!(
            "RPC request timeout after {:?} for get_multiple_accounts ({} accounts)",
            timeout_duration,
            pubkeys_count
        ))?
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
        let timeout_duration = self.request_timeout;
        
        let response = timeout(
            timeout_duration,
            tokio::task::spawn_blocking(move || {
                client.simulate_transaction(&tx)
            })
        )
        .await
        .map_err(|_| anyhow::anyhow!(
            "RPC request timeout after {:?} for simulate_transaction",
            timeout_duration
        ))?
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
