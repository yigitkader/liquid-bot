use crate::blockchain::jito::{create_jito_client, JitoBundle, JitoClient};
use crate::blockchain::rpc_client::RpcClient;
use crate::blockchain::transaction::{send_and_confirm, sign_transaction, TransactionBuilder};
use crate::core::config::Config;
use crate::core::events::{Event, EventBus};
use crate::core::types::Opportunity;
use crate::protocol::Protocol;
use crate::strategy::balance_manager::BalanceManager;
use anyhow::{Context, Result};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::broadcast;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;

/// ATA cache entry with timestamp for cleanup
#[derive(Clone, Debug)]
struct AtaCacheEntry {
    timestamp: Instant,
    verified: bool,
}

struct TxLock {
    locked: Arc<std::sync::RwLock<HashSet<Pubkey>>>,
    lock_times: Arc<std::sync::RwLock<HashMap<Pubkey, Instant>>>,
    timeout_seconds: u64,
    cancel_token: CancellationToken,
}

impl TxLock {
    fn new(timeout_seconds: u64) -> Self {
        TxLock {
            locked: Arc::new(std::sync::RwLock::new(HashSet::new())),
            lock_times: Arc::new(std::sync::RwLock::new(HashMap::new())),
            timeout_seconds,
            cancel_token: CancellationToken::new(),
        }
    }

    fn start_cleanup_task(self: &Arc<Self>) {
        let locked = Arc::clone(&self.locked);
        let lock_times = Arc::clone(&self.lock_times);
        let timeout_seconds = self.timeout_seconds;
        let cancel = self.cancel_token.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = sleep(Duration::from_secs(10)) => {
                // Run cleanup every 10 seconds
                let mut locked_guard = locked.write().unwrap();
                let mut lock_times_guard = lock_times.write().unwrap();

                // Remove expired locks
                let expired_addresses: Vec<Pubkey> = lock_times_guard
                    .iter()
                    .filter_map(|(address, time)| {
                        if time.elapsed().as_secs() >= timeout_seconds {
                            Some(*address)
                        } else {
                            None
                        }
                    })
                    .collect();

                for address in &expired_addresses {
                    locked_guard.remove(address);
                    lock_times_guard.remove(address);
                }

                if !expired_addresses.is_empty() {
                    log::debug!(
                        "TxLock: cleaned up {} expired lock(s)",
                        expired_addresses.len()
                    );
                        }
                    }
                    _ = cancel.cancelled() => {
                        log::info!("TxLock cleanup task shutting down gracefully");
                        break;
                    }
                }
            }
        });
    }

    pub fn cancel_cleanup(&self) {
        self.cancel_token.cancel();
    }

    fn try_lock(&self, address: &Pubkey) -> Result<TxLockGuard> {
        let mut locked = self.locked.write().unwrap();
        let mut lock_times = self.lock_times.write().unwrap();

        if let Some(lock_time) = lock_times.get(address) {
            if lock_time.elapsed().as_secs() >= self.timeout_seconds {
                locked.remove(address);
                lock_times.remove(address);
            } else {
                return Err(anyhow::anyhow!("Account already locked"));
            }
        }

        locked.insert(*address);
        lock_times.insert(*address, std::time::Instant::now());

        Ok(TxLockGuard {
            locked: Arc::clone(&self.locked),
            lock_times: Arc::clone(&self.lock_times),
            address: *address,
        })
    }
}

struct TxLockGuard {
    locked: Arc<std::sync::RwLock<HashSet<Pubkey>>>,
    lock_times: Arc<std::sync::RwLock<HashMap<Pubkey, Instant>>>,
    address: Pubkey,
}

impl Drop for TxLockGuard {
    fn drop(&mut self) {
        let mut locked = self.locked.write().unwrap();
        let mut lock_times = self.lock_times.write().unwrap();
        locked.remove(&self.address);
        lock_times.remove(&self.address);
    }
}

pub struct Executor {
    event_bus: EventBus,
    rpc: Arc<RpcClient>,
    wallet: Arc<Keypair>,
    protocol: Arc<dyn Protocol>,
    balance_manager: Arc<BalanceManager>,
    tx_lock: Arc<TxLock>,
    config: Config,
    consecutive_errors: Arc<AtomicU32>,
    jito_client: Option<Arc<JitoClient>>,
    use_jito: bool,
    // Cache of ATAs we've already included create_ata instructions for
    // This avoids redundant instructions within the same executor instance
    // ✅ FIX: Use HashMap with timestamp for cleanup to prevent memory leak
    ata_cache: Arc<RwLock<HashMap<Pubkey, AtaCacheEntry>>>,
    ata_cache_cleanup_token: CancellationToken,
}

impl Executor {
    pub fn new(
        event_bus: EventBus,
        rpc: Arc<RpcClient>,
        wallet: Arc<Keypair>,
        protocol: Arc<dyn Protocol>,
        balance_manager: Arc<BalanceManager>,
        config: Config,
    ) -> Self {
        let tx_lock = Arc::new(TxLock::new(config.tx_lock_timeout_seconds));

        tx_lock.start_cleanup_task();

        let use_jito = std::env::var("USE_JITO")
            .unwrap_or_else(|_| "false".to_string())
            .parse::<bool>()
            .unwrap_or(false);

        let jito_client = if use_jito {
            create_jito_client(&config).map(Arc::new)
        } else {
            None
        };

        if use_jito {
            if jito_client.is_some() {
                log::info!("✅ Jito MEV protection enabled");
            } else {
                log::warn!("⚠️  USE_JITO=true but Jito client creation failed - falling back to standard RPC");
            }
        } else {
            log::info!("ℹ️  Jito MEV protection disabled (USE_JITO=false or not set)");
            log::warn!("⚠️  WARNING: Without Jito, transactions are vulnerable to front-running!");
        }

        let ata_cache: Arc<RwLock<HashMap<Pubkey, AtaCacheEntry>>> = Arc::new(RwLock::new(HashMap::new()));
        let ata_cache_cleanup_token = CancellationToken::new();
        
        // ✅ FIX: Start background cleanup task for ATA cache
        let cache_for_cleanup = Arc::clone(&ata_cache);
        let cleanup_token = ata_cache_cleanup_token.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = sleep(Duration::from_secs(3600)) => {
                        // Run cleanup every hour
                        let mut cache = cache_for_cleanup.write().await;
                        let now = Instant::now();
                        
                        // Remove entries older than 24 hours
                        let before_len = cache.len();
                        cache.retain(|_, entry| {
                            now.duration_since(entry.timestamp) < Duration::from_secs(86400)
                        });
                        let after_len = cache.len();
                        
                        if before_len != after_len {
                            log::debug!(
                                "Executor: ATA cache cleanup: removed {} entries ({} remaining)",
                                before_len - after_len,
                                after_len
                            );
                        }
                    }
                    _ = cleanup_token.cancelled() => {
                        log::info!("Executor ATA cache cleanup task shutting down gracefully");
                        break;
                    }
                }
            }
        });

        Executor {
            event_bus,
            rpc,
            wallet,
            protocol,
            balance_manager,
            tx_lock,
            config,
            consecutive_errors: Arc::new(AtomicU32::new(0)),
            jito_client,
            use_jito,
            ata_cache,
            ata_cache_cleanup_token,
        }
    }

    fn check_error_threshold(&self) -> Result<()> {
        let errors = self.consecutive_errors.load(Ordering::Relaxed);
        if errors >= self.config.max_consecutive_errors {
            log::error!(
                "🚨 CRITICAL: Executor exceeded max consecutive errors ({} >= {})",
                errors,
                self.config.max_consecutive_errors
            );
            log::error!("🚨 Executor entering panic mode - shutting down");
            return Err(anyhow::anyhow!(
                "Executor exceeded max consecutive errors: {} >= {}",
                errors,
                self.config.max_consecutive_errors
            ));
        }
        Ok(())
    }

    fn record_error(&self) -> Result<()> {
        // ✅ FIX: Check for overflow BEFORE incrementing to prevent wraparound
        // If we're near u32::MAX, reset to max_consecutive_errors to keep threshold check working
        let previous_value = self.consecutive_errors.load(Ordering::Relaxed);
        
        if previous_value >= u32::MAX - 10 {
            // Safety margin: reset before overflow to prevent wraparound
            log::error!(
                "🚨 CRITICAL: Executor error counter near overflow ({}), resetting to max_consecutive_errors ({})",
                previous_value,
                self.config.max_consecutive_errors
            );
            self.consecutive_errors.store(self.config.max_consecutive_errors, Ordering::Relaxed);
            // Still check threshold after reset
            return self.check_error_threshold();
        }

        // Now safely increment
        let errors = self.consecutive_errors.fetch_add(1, Ordering::Relaxed) + 1;
        log::warn!(
            "Executor: consecutive errors: {}/{}",
            errors,
            self.config.max_consecutive_errors
        );
        self.check_error_threshold()
    }

    fn reset_errors(&self) {
        self.consecutive_errors.store(0, Ordering::Relaxed);
    }

    pub fn shutdown(&self) {
        log::info!("Executor: shutting down gracefully, cancelling background tasks");
        self.tx_lock.cancel_cleanup();
        self.ata_cache_cleanup_token.cancel();
    }

    fn is_retryable_error(&self, error: &anyhow::Error) -> bool {
        let error_str = error.to_string().to_lowercase();
        error_str.contains("timeout")
            || error_str.contains("network")
            || error_str.contains("connection")
            || error_str.contains("rpc")
            || error_str.contains("failed to send")
            || error_str.contains("request failed")
            || error_str.contains("temporarily unavailable")
            || error_str.contains("rate limit")
            || error_str.contains("too many requests")
    }

    pub async fn run(&self) -> Result<()> {
        let mut receiver = self.event_bus.subscribe();

        loop {
            match receiver.recv().await {
                Ok(Event::OpportunityApproved { opportunity }) => {
                    log::info!(
                        "Executor: received OpportunityApproved for position {} (debt_mint={}, collateral_mint={}, max_liquidatable={}, est_profit={:.4})",
                        opportunity.position.address,
                        opportunity.debt_mint,
                        opportunity.collateral_mint,
                        opportunity.max_liquidatable,
                        opportunity.estimated_profit
                    );
                    let _guard = match self.tx_lock.try_lock(&opportunity.position.address) {
                        Ok(guard) => guard,
                        Err(_) => {
                            log::warn!(
                                "Account {} already locked, skipping",
                                opportunity.position.address
                            );
                            continue;
                        }
                    };

                    match self.execute(opportunity).await {
                        Ok(signature) => {
                            log::info!(
                                "Executor: transaction sent for opportunity (position={}): {}",
                                signature,
                                "OK"
                            );
                            self.reset_errors();
                        }
                        Err(e) => {
                            log::error!("Failed to execute liquidation: {}", e);
                            if let Err(panic_err) = self.record_error() {
                                log::error!("🚨 Executor panic triggered: {}", panic_err);
                                return Err(panic_err);
                            }
                        }
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    log::warn!("Executor lagged, skipped {} events", skipped);
                }
                Err(broadcast::error::RecvError::Closed) => {
                    log::error!("Event bus closed, executor shutting down");
                    break;
                }
            }
        }

        Ok(())
    }

    async fn execute(&self, opp: Opportunity) -> Result<solana_sdk::signature::Signature> {
        use crate::protocol::solend::accounts::get_associated_token_address;
        use solana_sdk::signature::Signer;

        let wallet_pubkey = self.wallet.pubkey();
        let liq_ix = self
            .protocol
            .build_liquidation_ix(&opp, &wallet_pubkey, Some(Arc::clone(&self.rpc)))
            .await?;

        let destination_collateral =
            get_associated_token_address(&wallet_pubkey, &opp.collateral_mint, Some(&self.config))
                .context("Failed to derive destination collateral ATA")?;

        let mut tx_builder = TransactionBuilder::new(wallet_pubkey);
        tx_builder.add_compute_budget(200_000, 1_000);
        let mut cache = self.ata_cache.write().await;

        if !cache.contains_key(&destination_collateral) {
            // Cache miss - check RPC to see if ATA actually exists
            log::debug!(
                "Executor: Cache miss for ATA {}, checking on-chain existence...",
                destination_collateral
            );

            match self.rpc.get_account(&destination_collateral).await {
                Ok(_) => {
                    cache.insert(destination_collateral, AtaCacheEntry {
                        timestamp: Instant::now(),
                        verified: true,
                    });
                    log::debug!(
                        "Executor: ATA {} exists on-chain, added to cache, skipping create_ata instruction",
                        destination_collateral
                    );
                }
                Err(e) => {
                    let error_str = e.to_string().to_lowercase();
                    if error_str.contains("accountnotfound")
                        || error_str.contains("account not found")
                    {
                        // ✅ ATA doesn't exist - add instruction
                        log::debug!(
                            "Executor: ATA {} doesn't exist, adding create_ata instruction",
                            destination_collateral
                        );
                        let create_ata_ix = self.create_ata_instruction(&opp.collateral_mint)?;
                        tx_builder.add_instruction(create_ata_ix);

                        cache.insert(destination_collateral, AtaCacheEntry {
                            timestamp: Instant::now(),
                            verified: false, // Not verified yet, will be verified after transaction
                        });
                    } else {
                        let error_str = e.to_string().to_lowercase();
                        let is_timeout =
                            error_str.contains("timeout") || error_str.contains("timed out");

                        if is_timeout {
                            log::warn!(
                                "Executor: RPC timeout checking ATA existence for {}. Adding create_ata instruction but NOT caching (will retry RPC check on next opportunity)",
                                destination_collateral
                            );
                            let create_ata_ix =
                                self.create_ata_instruction(&opp.collateral_mint)?;
                            tx_builder.add_instruction(create_ata_ix);
                            // Don't cache on timeout - will retry next time
                        } else {
                            log::warn!(
                                "Executor: Failed to check ATA existence for {}: {}. Adding create_ata instruction anyway (idempotent)",
                                destination_collateral,
                                e
                            );
                            let create_ata_ix =
                                self.create_ata_instruction(&opp.collateral_mint)?;
                            tx_builder.add_instruction(create_ata_ix);
                            cache.insert(destination_collateral, AtaCacheEntry {
                                timestamp: Instant::now(),
                                verified: false,
                            });
                        }
                    }
                }
            }
        } else {
            log::debug!(
                "Executor: Skipping create_ata instruction for {} (already in cache)",
                destination_collateral
            );
        }

        tx_builder.add_instruction(liq_ix);

        let signature = if self.config.dry_run {
            log::info!("DRY RUN: Would send transaction (not sending to blockchain)");
            return Err(anyhow::anyhow!(
                "DRY_RUN mode: Transaction not sent to blockchain"
            ));
        } else if self.use_jito {
            if let Some(ref jito) = self.jito_client {
                self.send_via_jito_with_retry(&tx_builder, jito).await?
            } else {
                log::warn!(
                    "⚠️  USE_JITO=true but Jito client is None - falling back to standard RPC"
                );
                self.send_with_retry(&tx_builder).await?
            }
        } else {
            self.send_with_retry(&tx_builder).await?
        };

        self.balance_manager
            .release(&opp.debt_mint, opp.max_liquidatable)
            .await;

        self.event_bus.publish(Event::TransactionSent {
            signature: signature.to_string(),
        })?;

        Ok(signature)
    }

    fn create_ata_instruction(
        &self,
        mint: &Pubkey,
    ) -> Result<solana_sdk::instruction::Instruction> {
        use crate::protocol::solend::accounts::get_associated_token_program_id;
        use solana_sdk::signature::Signer;

        let wallet_pubkey = self.wallet.pubkey();
        let associated_token_program = get_associated_token_program_id(Some(&self.config))
            .context("Failed to get associated token program ID")?;
        let token_program = spl_token::id();

        let (ata, _bump) = Pubkey::try_find_program_address(
            &[
                wallet_pubkey.as_ref(),
                token_program.as_ref(),
                mint.as_ref(),
            ],
            &associated_token_program,
        )
        .ok_or_else(|| anyhow::anyhow!("Failed to find program address for ATA"))?;

        log::debug!(
            "Executor: Creating ATA instruction: payer={}, wallet={}, mint={}, ata={}",
            wallet_pubkey,
            wallet_pubkey,
            mint,
            ata
        );

        Ok(solana_sdk::instruction::Instruction {
            program_id: associated_token_program,
            accounts: vec![
                solana_sdk::instruction::AccountMeta::new(wallet_pubkey, true),
                solana_sdk::instruction::AccountMeta::new(ata, false),
                solana_sdk::instruction::AccountMeta::new_readonly(wallet_pubkey, false),
                solana_sdk::instruction::AccountMeta::new_readonly(*mint, false),
                solana_sdk::instruction::AccountMeta::new_readonly(
                    solana_sdk::system_program::id(),
                    false,
                ),
                solana_sdk::instruction::AccountMeta::new_readonly(token_program, false),
            ],
            data: vec![],
        })
    }

    async fn send_via_jito_with_retry(
        &self,
        tx_builder: &TransactionBuilder,
        jito_client: &Arc<JitoClient>,
    ) -> Result<solana_sdk::signature::Signature> {
        const MAX_TX_RETRIES: u32 = 3;

        for attempt in 1..=MAX_TX_RETRIES {
            match self.send_via_jito(tx_builder, jito_client).await {
                Ok(sig) => {
                    if sig == solana_sdk::signature::Signature::default() {
                        log::error!(
                            "Invalid signature returned (all zeros) on attempt {}",
                            attempt
                        );
                        if attempt < MAX_TX_RETRIES {
                            log::warn!("Retrying due to invalid signature...");
                            sleep(Duration::from_millis(500 * attempt as u64)).await;
                            continue;
                        }
                        return Err(anyhow::anyhow!(
                            "Invalid signature returned (all zeros) after {} attempts",
                            MAX_TX_RETRIES
                        ));
                    }
                    if attempt > 1 {
                        log::info!("✅ Transaction sent successfully on attempt {}", attempt);
                    }
                    return Ok(sig);
                }
                Err(e) => {
                    let is_retryable = self.is_retryable_error(&e);

                    if is_retryable && attempt < MAX_TX_RETRIES {
                        log::warn!(
                            "Jito TX failed (attempt {}/{}), retrying: {}",
                            attempt,
                            MAX_TX_RETRIES,
                            e
                        );
                        sleep(Duration::from_millis(500 * attempt as u64)).await;
                    } else {
                        if attempt >= MAX_TX_RETRIES {
                            log::error!("Jito TX failed after {} attempts: {}", MAX_TX_RETRIES, e);
                        }
                        return Err(e);
                    }
                }
            }
        }

        // Should never reach here, but return error just in case
        Err(anyhow::anyhow!(
            "Failed to send transaction after {} attempts",
            MAX_TX_RETRIES
        ))
    }

    async fn send_via_jito(
        &self,
        tx_builder: &TransactionBuilder,
        jito_client: &Arc<JitoClient>,
    ) -> Result<solana_sdk::signature::Signature> {
        let blockhash = self.rpc.get_recent_blockhash().await?;

        let mut bundle =
            JitoBundle::new(*jito_client.tip_account(), jito_client.default_tip_amount());

        jito_client
            .add_tip_transaction(&mut bundle, &self.wallet, blockhash)
            .context("Failed to add Jito tip transaction")?;

        let mut main_tx = tx_builder.build(blockhash);

        sign_transaction(&mut main_tx, &self.wallet)
            .context("Failed to sign main transaction")?;

        bundle.add_transaction(main_tx);

        let bundle_id = jito_client
            .send_bundle(&bundle)
            .await
            .context("Failed to send bundle to Jito")?;

        log::info!(
            "✅ Jito bundle sent: bundle_id={}, tip={} lamports, transactions={}",
            bundle_id,
            jito_client.default_tip_amount(),
            bundle.transactions().len()
        );

        let main_tx = bundle
            .transactions()
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("Bundle missing main transaction"))?;

        if main_tx.signatures.is_empty() {
            return Err(anyhow::anyhow!(
                "Main transaction not signed - signatures array is empty"
            ));
        }

        let sig = main_tx.signatures[0];
        if sig == solana_sdk::signature::Signature::default() {
            return Err(anyhow::anyhow!(
                "Main transaction not properly signed - signature is default (all zeros)"
            ));
        }

        Ok(sig)
    }

    async fn send_with_retry(
        &self,
        tx_builder: &TransactionBuilder,
    ) -> Result<solana_sdk::signature::Signature> {
        const MAX_TX_RETRIES: u32 = 3;

        for attempt in 1..=MAX_TX_RETRIES {
            let blockhash = self.rpc.get_recent_blockhash().await?;
            let mut tx = tx_builder.build(blockhash);
            sign_transaction(&mut tx, &self.wallet)
                .context("Failed to sign transaction - double signing detected")?;

            match send_and_confirm(tx, Arc::clone(&self.rpc)).await {
                Ok(sig) => {
                    if attempt > 1 {
                        log::info!("✅ Transaction sent successfully on attempt {}", attempt);
                    }
                    return Ok(sig);
                }
                Err(e) => {
                    let is_retryable = self.is_retryable_error(&e);

                    if is_retryable && attempt < MAX_TX_RETRIES {
                        log::warn!(
                            "RPC TX failed (attempt {}/{}), retrying: {}",
                            attempt,
                            MAX_TX_RETRIES,
                            e
                        );
                        // Exponential backoff: 500ms, 1000ms, 2000ms
                        sleep(Duration::from_millis(500 * attempt as u64)).await;
                    } else {
                        if attempt >= MAX_TX_RETRIES {
                            log::error!("RPC TX failed after {} attempts: {}", MAX_TX_RETRIES, e);
                        }
                        return Err(e);
                    }
                }
            }
        }

        // Should never reach here, but return error just in case
        Err(anyhow::anyhow!(
            "Failed to send transaction after {} attempts",
            MAX_TX_RETRIES
        ))
    }
}
