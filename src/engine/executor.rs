use crate::core::events::{Event, EventBus};
use crate::core::types::Opportunity;
use crate::blockchain::rpc_client::RpcClient;
use crate::blockchain::transaction::{TransactionBuilder, sign_transaction, send_and_confirm};
use crate::blockchain::jito::{JitoClient, JitoBundle, create_jito_client};
use crate::protocol::Protocol;
use crate::strategy::balance_manager::BalanceManager;
use crate::core::config::Config;
use solana_sdk::signature::Keypair;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::transaction::Transaction;
use anyhow::{Result, Context};
use std::sync::Arc;
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::broadcast;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;
use tokio::sync::RwLock;

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

    /// Start background cleanup task that periodically removes expired locks.
    /// This prevents locks from leaking when no new lock attempts are made.
    /// The task can be gracefully shut down using the cancellation token.
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

    /// Cancel the cleanup task (for graceful shutdown)
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
    ata_cache: Arc<RwLock<HashSet<Pubkey>>>,
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
        
        // Start background cleanup task to prevent lock leaks
        tx_lock.start_cleanup_task();
        
        // Check if Jito is enabled
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
            ata_cache: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    /// Check if we've exceeded max consecutive errors and panic if so
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

    /// Record a critical error and check threshold
    fn record_error(&self) -> Result<()> {
        let errors = self.consecutive_errors.fetch_add(1, Ordering::Relaxed) + 1;
        log::warn!(
            "Executor: consecutive errors: {}/{}",
            errors,
            self.config.max_consecutive_errors
        );
        self.check_error_threshold()
    }

    /// Reset error counter on successful operation
    fn reset_errors(&self) {
        self.consecutive_errors.store(0, Ordering::Relaxed);
    }

    /// Gracefully shutdown the executor, cancelling background tasks
    pub fn shutdown(&self) {
        log::info!("Executor: shutting down gracefully, cancelling background tasks");
        self.tx_lock.cancel_cleanup();
    }

    /// Check if an error is retryable (network errors, timeouts, etc.)
    fn is_retryable_error(&self, error: &anyhow::Error) -> bool {
        let error_str = error.to_string().to_lowercase();
        
        // Retryable errors: network issues, timeouts, RPC errors
        error_str.contains("timeout") ||
        error_str.contains("network") ||
        error_str.contains("connection") ||
        error_str.contains("rpc") ||
        error_str.contains("failed to send") ||
        error_str.contains("request failed") ||
        error_str.contains("temporarily unavailable") ||
        error_str.contains("rate limit") ||
        error_str.contains("too many requests")
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
                            log::warn!("Account {} already locked, skipping", opportunity.position.address);
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
                            // Reset errors on successful execution
                            self.reset_errors();
                        }
                        Err(e) => {
                            log::error!("Failed to execute liquidation: {}", e);
                            // Record critical error (transaction failure)
                            if let Err(panic_err) = self.record_error() {
                                log::error!("🚨 Executor panic triggered: {}", panic_err);
                                return Err(panic_err);
                            }
                        }
                    }
                }
                Ok(_) => {
                }
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
        use solana_sdk::signature::Signer;
        use crate::protocol::solend::accounts::get_associated_token_address;
        
        let wallet_pubkey = self.wallet.pubkey();
        let liq_ix = self.protocol
            .build_liquidation_ix(&opp, &wallet_pubkey, Some(Arc::clone(&self.rpc)))
            .await?;

        // Derive destination_collateral ATA address
        let destination_collateral = get_associated_token_address(
            &wallet_pubkey,
            &opp.collateral_mint,
            Some(&self.config),
        )
        .context("Failed to derive destination collateral ATA")?;
        
        let mut tx_builder = TransactionBuilder::new(wallet_pubkey);
        tx_builder.add_compute_budget(200_000, 1_000);
        
        // ✅ CRITICAL FIX: Always include create_ata instruction if not in cache
        // ATA creation is idempotent - if ATA already exists, instruction is a no-op
        // This eliminates race conditions and unnecessary RPC calls
        // Cache prevents redundant instructions within the same executor instance
        let should_add_ata = {
            let cache = self.ata_cache.read().await;
            !cache.contains(&destination_collateral)
        };
        
        if should_add_ata {
            log::debug!(
                "Executor: Adding create_ata instruction for destination_collateral ATA ({})",
                destination_collateral
            );
            let create_ata_ix = self.create_ata_instruction(&opp.collateral_mint)?;
            tx_builder.add_instruction(create_ata_ix);
            
            // Add to cache (even if creation fails, we don't want to retry immediately)
            let mut cache = self.ata_cache.write().await;
            cache.insert(destination_collateral);
        } else {
            log::debug!(
                "Executor: Skipping create_ata instruction for {} (already in cache)",
                destination_collateral
            );
        }
        
        tx_builder.add_instruction(liq_ix);

        let signature = if self.config.dry_run {
            log::info!("DRY RUN: Would send transaction (not sending to blockchain)");
            return Err(anyhow::anyhow!("DRY_RUN mode: Transaction not sent to blockchain"));
        } else if let Some(ref jito) = self.jito_client {
            // Send via Jito bundle for MEV protection with retries
            self.send_via_jito_with_retry(&tx_builder, jito).await?
        } else {
            // Fallback to standard RPC (vulnerable to front-running) with retries
            self.send_with_retry(&tx_builder).await?
        };

        self.balance_manager.release(&opp.debt_mint, opp.max_liquidatable).await;

        self.event_bus.publish(Event::TransactionSent {
            signature: signature.to_string(),
        })?;

        Ok(signature)
    }

    /// Create ATA instruction for a given mint
    fn create_ata_instruction(&self, mint: &Pubkey) -> Result<solana_sdk::instruction::Instruction> {
        use crate::protocol::solend::accounts::get_associated_token_program_id;
        use solana_sdk::signature::Signer;
        
        let wallet_pubkey = self.wallet.pubkey();
        let associated_token_program = get_associated_token_program_id(Some(&self.config))
            .context("Failed to get associated token program ID")?;
        let token_program = spl_token::id();

        let (ata, _bump) = Pubkey::try_find_program_address(
            &[wallet_pubkey.as_ref(), token_program.as_ref(), mint.as_ref()],
            &associated_token_program,
        ).ok_or_else(|| anyhow::anyhow!("Failed to find program address for ATA"))?;
        
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
            data: vec![], // Create instruction has no data
        })
    }

    /// Send transaction via Jito bundle for MEV protection with retry logic
    async fn send_via_jito_with_retry(
        &self,
        tx_builder: &TransactionBuilder,
        jito_client: &Arc<JitoClient>,
    ) -> Result<solana_sdk::signature::Signature> {
        const MAX_TX_RETRIES: u32 = 3;
        
        for attempt in 1..=MAX_TX_RETRIES {
            match self.send_via_jito(tx_builder, jito_client).await {
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
                            "Jito TX failed (attempt {}/{}), retrying: {}",
                            attempt,
                            MAX_TX_RETRIES,
                            e
                        );
                        // Exponential backoff: 500ms, 1000ms, 2000ms
                        sleep(Duration::from_millis(500 * attempt as u64)).await;
                    } else {
                        if attempt >= MAX_TX_RETRIES {
                            log::error!(
                                "Jito TX failed after {} attempts: {}",
                                MAX_TX_RETRIES,
                                e
                            );
                        }
                        return Err(e);
                    }
                }
            }
        }
        
        // Should never reach here, but return error just in case
        Err(anyhow::anyhow!("Failed to send transaction after {} attempts", MAX_TX_RETRIES))
    }

    /// Send transaction via Jito bundle for MEV protection
    /// 
    /// CRITICAL: This function creates a NEW transaction from the builder with a fresh blockhash.
    /// DO NOT pass an already-signed transaction, as changing blockhash and re-signing causes double signing.
    async fn send_via_jito(
        &self,
        tx_builder: &TransactionBuilder,
        jito_client: &Arc<JitoClient>,
    ) -> Result<solana_sdk::signature::Signature> {
        use solana_sdk::signature::Signer;
        
        // Get fresh blockhash for this bundle
        let blockhash = self.rpc.get_recent_blockhash().await?;
        
        // Create bundle
        let mut bundle = JitoBundle::new(
            *jito_client.tip_account(),
            jito_client.default_tip_amount(),
        );
        
        // Add tip transaction (must be first)
        jito_client.add_tip_transaction(
            &mut bundle,
            &self.wallet,
            blockhash,
        )
        .context("Failed to add Jito tip transaction")?;
        
        // ✅ CRITICAL FIX: Build a FRESH, UNSIGNED transaction from scratch with the fresh blockhash
        // TransactionBuilder::build() creates an UNSIGNED transaction - this is correct!
        // We sign it ONCE here, and sign_transaction() will prevent double-signing if called again
        // DO NOT pass an already-signed transaction here - always build fresh!
        let mut main_tx = tx_builder.build(blockhash);
        
        // Sign the transaction ONCE - sign_transaction() has guards to prevent double-signing
        sign_transaction(&mut main_tx, &self.wallet);
        
        // Add main liquidation transaction
        bundle.add_transaction(main_tx);
        
        // Send bundle to Jito
        let bundle_id = jito_client.send_bundle(&bundle).await
            .context("Failed to send bundle to Jito")?;
        
        log::info!(
            "✅ Jito bundle sent: bundle_id={}, tip={} lamports, transactions={}",
            bundle_id,
            jito_client.default_tip_amount(),
            bundle.transactions().len()
        );
        
        // Return the signature of the main transaction (first non-tip transaction)
        // Note: In production, you might want to track bundle status separately
        let main_tx = bundle.transactions().get(1)
            .ok_or_else(|| anyhow::anyhow!("Bundle missing main transaction"))?;

        // Transaction henüz sign edilmemiş olabilir
        if main_tx.signatures.is_empty() {
            return Err(anyhow::anyhow!("Main transaction not signed"));
        }

            Ok(main_tx.signatures[0])
    }

    /// Send transaction via standard RPC with retry logic
    async fn send_with_retry(
        &self,
        tx_builder: &TransactionBuilder,
    ) -> Result<solana_sdk::signature::Signature> {
        const MAX_TX_RETRIES: u32 = 3;
        
        for attempt in 1..=MAX_TX_RETRIES {
            // Rebuild transaction with fresh blockhash for each retry attempt
            let blockhash = self.rpc.get_recent_blockhash().await?;
            let mut tx = tx_builder.build(blockhash);
            sign_transaction(&mut tx, &self.wallet);
            
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
                            log::error!(
                                "RPC TX failed after {} attempts: {}",
                                MAX_TX_RETRIES,
                                e
                            );
                        }
                        return Err(e);
                    }
                }
            }
        }
        
        // Should never reach here, but return error just in case
        Err(anyhow::anyhow!("Failed to send transaction after {} attempts", MAX_TX_RETRIES))
    }
}
