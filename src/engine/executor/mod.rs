// Executor modülleri
pub mod tx_lock;
pub mod ata_cache;
pub mod wsol_handler;

pub use tx_lock::{TxLock, TxLockGuard};
pub use ata_cache::{AtaCache, AtaCacheEntry};
pub use wsol_handler::WsolHandler;

use crate::blockchain::jito::{create_jito_client, JitoBundle, JitoClient};
use crate::blockchain::rpc_client::RpcClient;
use crate::blockchain::transaction::{send_and_confirm, sign_transaction, TransactionBuilder};
use crate::core::config::Config;
use crate::core::events::{Event, EventBus};
use crate::core::types::Opportunity;
use crate::protocol::Protocol;
use crate::strategy::balance_manager::BalanceManager;
use crate::utils::error_tracker::ErrorTracker;
use anyhow::{Context, Result};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::broadcast;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

pub struct Executor {
    event_bus: EventBus,
    rpc: Arc<RpcClient>,
    wallet: Arc<Keypair>,
    protocol: Arc<dyn Protocol>,
    balance_manager: Arc<BalanceManager>,
    tx_lock: Arc<TxLock>,
    config: Config,
    error_tracker: ErrorTracker,
    jito_client: Option<Arc<JitoClient>>,
    use_jito: bool,
    ata_cache: AtaCache,
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
                log::info!("Jito MEV protection enabled");
            } else {
                log::warn!("USE_JITO=true but Jito client creation failed - falling back to standard RPC");
            }
        } else {
            log::info!("Jito MEV protection disabled");
            log::warn!("Without Jito, transactions are vulnerable to front-running");
        }

        let (ata_cache, ata_cache_cleanup_token) = AtaCache::new();

        let error_tracker = ErrorTracker::new(
            config.max_consecutive_errors,
            "Executor",
        );
        Executor {
            event_bus,
            rpc,
            wallet,
            protocol,
            balance_manager,
            tx_lock,
            config,
            error_tracker,
            jito_client,
            use_jito,
            ata_cache,
            ata_cache_cleanup_token,
        }
    }

    fn record_error(&self) -> Result<()> {
        self.error_tracker.record_error()
    }

    fn reset_errors(&self) {
        self.error_tracker.reset_errors();
    }

    // Helper methods for cache compatibility
    fn evict_lru_if_needed(cache: &mut HashMap<Pubkey, AtaCacheEntry>) {
        const ATA_CACHE_MAX_SIZE: usize = 50000;
        
        if cache.len() >= ATA_CACHE_MAX_SIZE {
            let mut entries: Vec<(Pubkey, AtaCacheEntry)> = cache.drain().collect();
            // Sort by last_accessed (least-recently-used first)
            entries.sort_by(|a, b| a.1.last_accessed.cmp(&b.1.last_accessed));
            // Keep only the most-recently-used entries
            let to_keep = ATA_CACHE_MAX_SIZE.saturating_sub(1000); // Keep 1k below max for buffer
            let entries_len = entries.len();
            let skip_count = entries_len.saturating_sub(to_keep);
            for (pubkey, entry) in entries.into_iter().skip(skip_count) {
                cache.insert(pubkey, entry);
            }
        }
    }

    fn get_cache_entry(cache: &mut HashMap<Pubkey, AtaCacheEntry>, key: &Pubkey) -> Option<AtaCacheEntry> {
        if let Some(entry) = cache.get_mut(key) {
            entry.last_accessed = Instant::now();
            Some(entry.clone())
        } else {
            None
        }
    }

    pub fn shutdown(&self) {
        log::info!("Executor: shutting down gracefully, cancelling background tasks");
        self.tx_lock.cancel_cleanup();
        self.ata_cache.cancel_cleanup();
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
                    let _guard = match self.tx_lock.try_lock(&opportunity.position.address).await {
                        Ok(guard) => guard,
                        Err(_) => {
                            log::warn!(
                                "Account {} already locked, skipping",
                                opportunity.position.address
                            );
                            continue;
                        }
                    };

                    match self.execute(opportunity.clone()).await {
                        Ok(signature) => {
                            log::info!(
                                "Executor: Transaction sent successfully for position {} (debt={}, collateral={}, est_profit=${:.4}, max_liquidatable={}): signature={}",
                                opportunity.position.address,
                                opportunity.debt_mint,
                                opportunity.collateral_mint,
                                opportunity.estimated_profit,
                                opportunity.max_liquidatable,
                                signature
                            );
                            self.reset_errors();
                        }
                        Err(e) => {
                    log::error!(
                        "Executor: Failed to execute liquidation for position {} (debt={}, collateral={}, est_profit=${:.4}): {}",
                        opportunity.position.address, opportunity.debt_mint, opportunity.collateral_mint,
                        opportunity.estimated_profit, e
                    );
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
        
        use crate::protocol::solend::instructions::is_wsol_mint;
        let source_liquidity_ata = get_associated_token_address(&wallet_pubkey, &opp.debt_mint, Some(&self.config))
            .context("Failed to derive source liquidity ATA")?;
        let destination_collateral = get_associated_token_address(&wallet_pubkey, &opp.collateral_mint, Some(&self.config))
            .context("Failed to derive destination collateral ATA")?;
        let cache_arc = self.ata_cache.cache();
        let mut cache = cache_arc.write().await;
        
        // Check and create source liquidity ATA (for WSOL) if needed
        if is_wsol_mint(&opp.debt_mint) {
            let needs_wsol_ata = match Self::get_cache_entry(&mut cache, &source_liquidity_ata) {
                Some(entry) => !entry.verified,
                None => true,
            };
            
            if needs_wsol_ata {
                match self.rpc.get_account(&source_liquidity_ata).await {
                    Ok(_) => {
                        Self::evict_lru_if_needed(&mut cache);
                        let now = Instant::now();
                        cache.insert(source_liquidity_ata, AtaCacheEntry {
                            created_at: now,
                            last_accessed: now,
                            verified: true,
                        });
                    }
                    Err(_) => {
                        // WSOL ATA doesn't exist - create in separate transaction
                        log::info!(
                            "Executor: WSOL ATA {} doesn't exist, creating in separate transaction",
                            source_liquidity_ata
                        );
                        self.create_ata_separate_tx(&opp.debt_mint, &source_liquidity_ata, &mut cache).await?;
                    }
                }
            }
        }
        
        // Check and create destination collateral ATA if needed
        let needs_rpc_check = match Self::get_cache_entry(&mut cache, &destination_collateral) {
            Some(entry) => !entry.verified,
            None => true,
        };
        
        if needs_rpc_check {
            match self.rpc.get_account(&destination_collateral).await {
                Ok(_) => {
                    Self::evict_lru_if_needed(&mut cache);
                    let now = Instant::now();
                    cache.insert(destination_collateral, AtaCacheEntry {
                        created_at: now,
                        last_accessed: now,
                        verified: true,
                    });
                }
                Err(e) => {
                    let error_str = e.to_string().to_lowercase();
                    if error_str.contains("accountnotfound") || error_str.contains("account not found") {
                        // ATA doesn't exist - create in separate transaction
                        log::info!(
                            "Executor: Destination collateral ATA {} doesn't exist, creating in separate transaction",
                            destination_collateral
                        );
                        self.create_ata_separate_tx(&opp.collateral_mint, &destination_collateral, &mut cache).await?;
                    } else {
                        // For other errors (timeout, etc.), add to liquidation tx as fallback
                        // This is safe because create_ata is idempotent
                        log::warn!(
                            "Executor: Failed to check ATA {} existence: {}. Will add to liquidation transaction as fallback",
                            destination_collateral,
                            e
                        );
                        Self::evict_lru_if_needed(&mut cache);
                        let now = Instant::now();
                        cache.insert(destination_collateral, AtaCacheEntry {
                            created_at: now,
                            last_accessed: now,
                            verified: false,
                        });
                    }
                }
            }
        }
        
        // ✅ CRITICAL FIX: Convert max_liquidatable from USD×1e6 to token amount
        // Problem: opp.max_liquidatable is in USD×1e6 (micro-USD), but we need token amount
        // Validator reserves using token amount, so Executor must use the same
        // Solution: Use same conversion logic as Validator to ensure consistency
        let max_liquidatable_token_amount = crate::utils::helpers::convert_usd_to_token_amount_for_mint(
            &opp.debt_mint,
            opp.max_liquidatable,
            &self.rpc,
            &self.config,
        )
        .await
        .context("Failed to convert max_liquidatable from USD to token amount")?;
        
        log::debug!(
            "Executor: Converted max_liquidatable: USD×1e6={}, token_amount={} for debt_mint={}",
            opp.max_liquidatable,
            max_liquidatable_token_amount,
            opp.debt_mint
        );
        
        let mut tx_builder = TransactionBuilder::new(wallet_pubkey);
        tx_builder.add_compute_budget(200_000, 1_000);
        
        if is_wsol_mint(&opp.debt_mint) {
            WsolHandler::add_wrap_instructions_if_needed(
                &wallet_pubkey,
                &source_liquidity_ata,
                max_liquidatable_token_amount,
                &self.rpc,
                &self.config,
                &mut tx_builder,
            )
            .await
            .context("Failed to add WSOL wrap instructions")?;
        }
        
        let liq_ix = self
            .protocol
            .build_liquidation_ix(&opp, &wallet_pubkey, Some(Arc::clone(&self.rpc)))
            .await?;
        
        if let Some(entry) = Self::get_cache_entry(&mut cache, &destination_collateral) {
            if !entry.verified {
                        log::warn!("Executor: Adding create_ata instruction as fallback for {} (not verified)", destination_collateral);
                        let create_ata_ix = crate::utils::ata_manager::create_ata_instruction(
                            &wallet_pubkey,
                            &wallet_pubkey,
                            &opp.collateral_mint,
                            Some(&self.config),
                            Some(&self.rpc),
                        ).await?;
                        tx_builder.add_instruction(create_ata_ix);
            }
        }

        tx_builder.add_instruction(liq_ix);

        let signature = self.send_transaction(&tx_builder).await?;

        // ✅ CRITICAL FIX: Release using token amount, not USD amount
        // Validator reserves using token amount (max_liquidatable_token_amount),
        // so we must release the same amount to maintain consistency
        self.balance_manager
            .release(&opp.debt_mint, max_liquidatable_token_amount)
            .await;

        self.event_bus.publish(Event::TransactionSent {
            signature: signature.to_string(),
        })?;

        if self.config.unwrap_wsol_after_liquidation && is_wsol_mint(&opp.debt_mint) {
            tokio::time::sleep(Duration::from_millis(500)).await;
            
            use crate::utils::helpers::read_ata_balance;
            let wsol_balance = match read_ata_balance(&source_liquidity_ata, &self.rpc).await {
                Ok(balance) => balance,
                Err(e) => {
                    log::warn!("Executor: Failed to read WSOL ATA balance for unwrap check: {}. Skipping unwrap.", e);
                    return Ok(signature);
                }
            };
            
            if wsol_balance > 0 {
                log::warn!("Executor: WSOL ATA has remaining balance: {} lamports, cannot unwrap", wsol_balance);
            } else {
                log::debug!("Executor: WSOL ATA balance is 0, attempting unwrap (WSOL ATA: {})", source_liquidity_ata);
                
                use crate::protocol::solend::instructions::build_unwrap_sol_instruction;
                match build_unwrap_sol_instruction(&wallet_pubkey, &source_liquidity_ata) {
                    Ok(unwrap_ix) => {
                        let mut unwrap_tx_builder = TransactionBuilder::new(wallet_pubkey);
                        unwrap_tx_builder.add_compute_budget(200_000, 1_000);
                        unwrap_tx_builder.add_instruction(unwrap_ix);
                        
                        match self.send_transaction(&unwrap_tx_builder).await {
                            Ok(unwrap_sig) => {
                                log::info!("Executor: Successfully unwrapped WSOL to native SOL (signature: {})", unwrap_sig);
                            }
                            Err(e) => {
                                log::warn!("Executor: Failed to send WSOL unwrap transaction (optional): {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("Executor: Failed to build WSOL unwrap instruction (optional): {}", e);
                    }
                }
            }
        }

        Ok(signature)
    }

    async fn send_transaction(&self, tx_builder: &TransactionBuilder) -> Result<solana_sdk::signature::Signature> {
        if self.config.dry_run {
            log::info!("DRY RUN: Would send transaction (not sending to blockchain)");
            return Err(anyhow::anyhow!("DRY_RUN mode: Transaction not sent to blockchain"));
        }
        if self.use_jito {
            if let Some(ref jito) = self.jito_client {
                self.send_via_jito_with_retry(tx_builder, jito).await
            } else {
                log::warn!("⚠️  USE_JITO=true but Jito client is None - falling back to standard RPC");
                self.send_with_retry(tx_builder).await
            }
        } else {
            self.send_with_retry(tx_builder).await
        }
    }

    async fn create_ata_separate_tx(
        &self,
        mint: &Pubkey,
        ata: &Pubkey,
        cache: &mut HashMap<Pubkey, AtaCacheEntry>,
    ) -> Result<()> {
        log::info!(
            "Executor: Creating ATA {} for mint {} in separate transaction",
            ata,
            mint
        );
        
        use solana_sdk::signature::Signer;
        let wallet_pubkey = self.wallet.pubkey();
        let create_ata_ix = crate::utils::ata_manager::create_ata_instruction(
            &wallet_pubkey,
            &wallet_pubkey,
            mint,
            Some(&self.config),
            Some(&self.rpc),
        ).await?;
        let mut ata_tx_builder = TransactionBuilder::new(wallet_pubkey);
        ata_tx_builder.add_compute_budget(200_000, 1_000);
        ata_tx_builder.add_instruction(create_ata_ix);
        
        let ata_sig = if self.config.dry_run {
            log::info!("DRY RUN: Would send ATA creation transaction");
            return Ok(());
        } else {
            self.send_transaction(&ata_tx_builder).await.context("Failed to send ATA creation transaction")?
        };
        
        log::info!(
            "Executor: ATA creation transaction sent: {} (waiting for confirmation...)",
            ata_sig
        );
        
        const MAX_VERIFICATION_ATTEMPTS: u32 = 10;
        const VERIFICATION_DELAY_MS: u64 = 500;
        
        for attempt in 1..=MAX_VERIFICATION_ATTEMPTS {
            match self.rpc.get_account(ata).await {
                Ok(_) => {
                    log::info!("Executor: ATA {} verified after {} attempt(s)", ata, attempt);
                    Self::evict_lru_if_needed(cache);
                    let now = Instant::now();
                    cache.insert(*ata, AtaCacheEntry {
                        created_at: now,
                        last_accessed: now,
                        verified: true,
                    });
                    return Ok(());
                }
                Err(e) => {
                    if attempt < MAX_VERIFICATION_ATTEMPTS {
                        log::debug!("Executor: ATA {} verification attempt {} failed, retrying...: {}", ata, attempt, e);
                        tokio::time::sleep(Duration::from_millis(VERIFICATION_DELAY_MS)).await;
                    } else {
                        log::warn!("Executor: ATA {} verification failed after {} attempts: {}", ata, MAX_VERIFICATION_ATTEMPTS, e);
                        log::info!("Executor: Checking transaction signature status for {}...", ata_sig);
                        
                        match self.rpc.get_signature_status(&ata_sig).await {
                            Ok(Some(true)) => {
                                log::info!("Executor: ATA creation transaction {} confirmed successfully, waiting longer for ATA to appear...", ata_sig);
                                const EXTENDED_WAIT_ATTEMPTS: u32 = 10;
                                const EXTENDED_WAIT_DELAY_MS: u64 = 1000;
                                
                                for extended_attempt in 1..=EXTENDED_WAIT_ATTEMPTS {
                                    match self.rpc.get_account(ata).await {
                                        Ok(_) => {
                                            log::info!("Executor: ATA {} verified after extended wait (attempt {})", ata, extended_attempt);
                                            Self::evict_lru_if_needed(cache);
                                            let now = Instant::now();
                                            cache.insert(*ata, AtaCacheEntry {
                                                created_at: now,
                                                last_accessed: now,
                                                verified: true,
                                            });
                                            return Ok(());
                                        }
                                        Err(e) => {
                                            if extended_attempt < EXTENDED_WAIT_ATTEMPTS {
                                                log::debug!("Executor: ATA {} extended wait attempt {} failed, retrying...: {}", ata, extended_attempt, e);
                                                tokio::time::sleep(Duration::from_millis(EXTENDED_WAIT_DELAY_MS)).await;
                                            } else {
                                                log::warn!("Executor: ATA {} still not found after extended wait ({} attempts), but transaction confirmed", ata, EXTENDED_WAIT_ATTEMPTS);
                                                Self::evict_lru_if_needed(cache);
                                                let now = Instant::now();
                                                cache.insert(*ata, AtaCacheEntry {
                                                    created_at: now,
                                                    last_accessed: now,
                                                    verified: true,
                                                });
                                                return Ok(());
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(Some(false)) => {
                                log::error!("Executor: ATA creation transaction {} FAILED", ata_sig);
                                return Err(anyhow::anyhow!("ATA creation transaction {} failed. ATA {} was not created.", ata_sig, ata));
                            }
                            Ok(None) => {
                                log::warn!("Executor: ATA creation transaction {} not found in history (may still be processing)", ata_sig);
                                const EXTENDED_WAIT_ATTEMPTS: u32 = 10;
                                const EXTENDED_WAIT_DELAY_MS: u64 = 1000;
                                
                                for extended_attempt in 1..=EXTENDED_WAIT_ATTEMPTS {
                                    match self.rpc.get_account(ata).await {
                                        Ok(_) => {
                                            log::info!("Executor: ATA {} verified after extended wait (attempt {})", ata, extended_attempt);
                                            Self::evict_lru_if_needed(cache);
                                            let now = Instant::now();
                                            cache.insert(*ata, AtaCacheEntry {
                                                created_at: now,
                                                last_accessed: now,
                                                verified: true,
                                            });
                                            return Ok(());
                                        }
                                        Err(e) => {
                                            if extended_attempt < EXTENDED_WAIT_ATTEMPTS {
                                                log::debug!("Executor: ATA {} extended wait attempt {} failed, retrying...: {}", ata, extended_attempt, e);
                                                tokio::time::sleep(Duration::from_millis(EXTENDED_WAIT_DELAY_MS)).await;
                                            } else {
                                                log::warn!("Executor: ATA {} still not found after extended wait ({} attempts). Will add to liquidation transaction as fallback.", ata, EXTENDED_WAIT_ATTEMPTS);
                                                Self::evict_lru_if_needed(cache);
                                                let now = Instant::now();
                                                cache.insert(*ata, AtaCacheEntry {
                                                    created_at: now,
                                                    last_accessed: now,
                                                    verified: false,
                                                });
                                                return Ok(());
                                            }
                                        }
                                    }
                                }
                            }
                            Err(status_err) => {
                                log::warn!("Executor: Failed to check transaction signature status for {}: {}. Will add to liquidation transaction as fallback.", ata_sig, status_err);
                                Self::evict_lru_if_needed(cache);
                                let now = Instant::now();
                                cache.insert(*ata, AtaCacheEntry {
                                    created_at: now,
                                    last_accessed: now,
                                    verified: false,
                                });
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }
        
        Ok(())
    }


    async fn send_via_jito_with_retry(
        &self,
        tx_builder: &TransactionBuilder,
        jito_client: &Arc<JitoClient>,
    ) -> Result<solana_sdk::signature::Signature> {
        use crate::utils::error_helpers::{retry_with_validation, RetryConfig};
        
        retry_with_validation(
            || async { self.send_via_jito(tx_builder, jito_client).await },
            |sig: &solana_sdk::signature::Signature| *sig != solana_sdk::signature::Signature::default(),
            RetryConfig::for_transaction(),
            "Jito TX",
        ).await
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
        use crate::utils::error_helpers::{retry_with_backoff, RetryConfig};
        
        retry_with_backoff(
            || async {
                let blockhash = self.rpc.get_recent_blockhash().await?;
                let mut tx = tx_builder.build(blockhash);
                sign_transaction(&mut tx, &self.wallet)
                    .context("Failed to sign transaction - double signing detected")?;
                send_and_confirm(tx, Arc::clone(&self.rpc)).await
            },
            RetryConfig::for_transaction(),
        ).await
    }
}
