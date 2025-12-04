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
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::broadcast;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
struct AtaCacheEntry {
    created_at: Instant,
    last_accessed: Instant,
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
                if let Ok(mut locked_guard) = locked.write() {
                    if let Ok(mut lock_times_guard) = lock_times.write() {
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
                            log::debug!("TxLock: cleaned up {} expired lock(s)", expired_addresses.len());
                        }
                    } else {
                        log::warn!("TxLock: lock_times RwLock is poisoned, skipping cleanup cycle");
                    }
                } else {
                    log::warn!("TxLock: locked RwLock is poisoned, skipping cleanup cycle");
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
        let mut locked = self.locked.write()
            .map_err(|_| anyhow::anyhow!("Lock is poisoned - cannot acquire lock"))?;
        let mut lock_times = self.lock_times.write()
            .map_err(|_| anyhow::anyhow!("Lock times is poisoned - cannot acquire lock"))?;

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
        if let Ok(mut locked) = self.locked.write() {
            locked.remove(&self.address);
        }
        if let Ok(mut lock_times) = self.lock_times.write() {
            lock_times.remove(&self.address);
        }
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
    error_tracker: ErrorTracker,
    jito_client: Option<Arc<JitoClient>>,
    use_jito: bool,
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
                log::info!("Jito MEV protection enabled");
            } else {
                log::warn!("USE_JITO=true but Jito client creation failed - falling back to standard RPC");
            }
        } else {
            log::info!("Jito MEV protection disabled");
            log::warn!("Without Jito, transactions are vulnerable to front-running");
        }

        let ata_cache: Arc<RwLock<HashMap<Pubkey, AtaCacheEntry>>> = Arc::new(RwLock::new(HashMap::new()));
        let ata_cache_cleanup_token = CancellationToken::new();
        
        const ATA_CACHE_TTL_SECONDS: u64 = 1 * 3600;
        const ATA_CACHE_MAX_SIZE: usize = 50000;
        const CLEANUP_INTERVAL_SECONDS: u64 = 300;
        
        let cache_for_cleanup = Arc::clone(&ata_cache);
        let cleanup_token = ata_cache_cleanup_token.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = sleep(Duration::from_secs(CLEANUP_INTERVAL_SECONDS)) => {
                        let mut cache = cache_for_cleanup.write().await;
                        let now = Instant::now();
                        let before_len = cache.len();
                        
                        cache.retain(|_, entry| {
                            now.duration_since(entry.created_at) < Duration::from_secs(ATA_CACHE_TTL_SECONDS)
                        });
                        
                        if cache.len() > ATA_CACHE_MAX_SIZE {
                            let mut entries: Vec<(Pubkey, AtaCacheEntry)> = cache.drain().collect();
                            entries.sort_by(|a, b| a.1.last_accessed.cmp(&b.1.last_accessed));
                            let to_remove = entries.len().saturating_sub(ATA_CACHE_MAX_SIZE);
                            let removed_count = to_remove;
                            for (pubkey, entry) in entries.into_iter().skip(to_remove) {
                                cache.insert(pubkey, entry);
                            }
                            
                            log::warn!(
                                "Executor: ATA cache exceeded max size ({}), removed {} least-recently-used entries (now: {})",
                                ATA_CACHE_MAX_SIZE,
                                removed_count,
                                cache.len()
                            );
                        }
                        
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

    fn check_error_threshold(&self) -> Result<()> {
        self.error_tracker.check_error_threshold()
    }

    fn record_error(&self) -> Result<()> {
        self.error_tracker.record_error()
    }

    fn reset_errors(&self) {
        self.error_tracker.reset_errors();
    }

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
        self.ata_cache_cleanup_token.cancel();
    }

    /// Check if error is retryable (delegates to shared utility)
    /// 
    /// Note: This method delegates to the shared `error_helpers::is_retryable_error` function.
    fn is_retryable_error(&self, error: &anyhow::Error) -> bool {
        crate::utils::error_helpers::is_retryable_error(error)
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
        
        use crate::protocol::solend::instructions::{is_wsol_mint, build_wrap_sol_instruction};
        let source_liquidity_ata = get_associated_token_address(&wallet_pubkey, &opp.debt_mint, Some(&self.config))
            .context("Failed to derive source liquidity ATA")?;
        let destination_collateral = get_associated_token_address(&wallet_pubkey, &opp.collateral_mint, Some(&self.config))
            .context("Failed to derive destination collateral ATA")?;
        let mut cache = self.ata_cache.write().await;
        
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
        
        let mut tx_builder = TransactionBuilder::new(wallet_pubkey);
        tx_builder.add_compute_budget(200_000, 1_000);
        
        if is_wsol_mint(&opp.debt_mint) {
            use crate::utils::helpers::read_ata_balance;
            let wsol_balance = read_ata_balance(&source_liquidity_ata, &self.rpc)
                .await
                .unwrap_or(0);
            
            let needed_wsol = opp.max_liquidatable;
            if wsol_balance < needed_wsol {
                let wrap_amount = needed_wsol.saturating_sub(wsol_balance);
                
                let wallet_account = self.rpc.get_account(&wallet_pubkey).await
                    .context("Failed to fetch wallet account for native SOL balance check")?;
                let native_sol_balance = wallet_account.lamports;
                let min_reserve = self.config.min_reserve_lamports;
                let required_sol = wrap_amount
                    .checked_add(min_reserve)
                    .ok_or_else(|| anyhow::anyhow!("Wrap amount + reserve overflow"))?;
                
                if native_sol_balance < required_sol {
                    return Err(anyhow::anyhow!(
                        "Insufficient native SOL for WSOL wrap: need {} lamports (wrap: {} + reserve: {}), have {} lamports",
                        required_sol,
                        wrap_amount,
                        min_reserve,
                        native_sol_balance
                    )).context("Cannot wrap SOL to WSOL - insufficient native SOL balance");
                }
                
                log::info!(
                    "Executor: Wrapping {} lamports of native SOL to WSOL (current WSOL balance: {}, needed: {}, native SOL: {})",
                    wrap_amount,
                    wsol_balance,
                    needed_wsol,
                    native_sol_balance
                );
                
                let wrap_instructions = build_wrap_sol_instruction(
                    &wallet_pubkey,
                    &source_liquidity_ata,
                    wrap_amount,
                    Some(&self.rpc),
                    Some(&self.config),
                )
                .await
                .context("Failed to build WSOL wrap instructions")?;
                for wrap_ix in wrap_instructions {
                    tx_builder.add_instruction(wrap_ix);
                }
            } else {
                log::debug!("Executor: Sufficient WSOL balance ({} >= {}), no wrap needed", wsol_balance, needed_wsol);
            }
        }
        
        let liq_ix = self
            .protocol
            .build_liquidation_ix(&opp, &wallet_pubkey, Some(Arc::clone(&self.rpc)))
            .await?;
        
        if let Some(entry) = Self::get_cache_entry(&mut cache, &destination_collateral) {
            if !entry.verified {
                log::warn!("Executor: Adding create_ata instruction as fallback for {} (not verified)", destination_collateral);
                let create_ata_ix = self.create_ata_instruction(&opp.collateral_mint).await?;
                tx_builder.add_instruction(create_ata_ix);
            }
        }

        tx_builder.add_instruction(liq_ix);

        let signature = self.send_transaction(&tx_builder).await?;

        self.balance_manager
            .release(&opp.debt_mint, opp.max_liquidatable)
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
        
        let create_ata_ix = self.create_ata_instruction(mint).await?;
        use solana_sdk::signature::Signer;
        let mut ata_tx_builder = TransactionBuilder::new(self.wallet.pubkey());
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

    async fn create_ata_instruction(
        &self,
        mint: &Pubkey,
    ) -> Result<solana_sdk::instruction::Instruction> {
        use crate::protocol::solend::accounts::get_associated_token_program_id;
        use crate::utils::ata_manager::get_token_program_for_mint;
        use solana_sdk::signature::Signer;

        let wallet_pubkey = self.wallet.pubkey();
        let associated_token_program = get_associated_token_program_id(Some(&self.config))
            .context("Failed to get associated token program ID")?;
        
        // ✅ FIX: Determine correct token program based on mint
        // Some mints use Token-2022 (Token Extensions), others use standard SPL Token
        // Check mint account owner on-chain for accurate determination
        let token_program = get_token_program_for_mint(mint, Some(&self.rpc))
            .await
            .context("Failed to determine token program for mint")?;

        // Always use manual construction to ensure correct account order and metadata
        // SPL library may have compatibility issues with certain Solana SDK versions
        let (ata, _bump) = Pubkey::try_find_program_address(
            &[
                wallet_pubkey.as_ref(),
                token_program.as_ref(),
                mint.as_ref(),
            ],
            &associated_token_program,
        )
        .ok_or_else(|| anyhow::anyhow!("Failed to find program address for ATA"))?;

        log::info!(
            "🔨 Executor: Creating ATA instruction: payer={}, wallet={}, mint={}, ata={}, token_program={}, program={}",
            wallet_pubkey,
            wallet_pubkey,
            mint,
            ata,
            token_program,
            associated_token_program
        );

        // Associated Token Program Create instruction format:
        // Accounts (in order):
        // 0. Payer (signer, writable) - pays for account creation
        // 1. ATA account (writable) - the account being created
        // 2. Owner (readonly) - the wallet that will own the ATA
        // 3. Mint (readonly) - the token mint
        // 4. System Program (readonly) - for account creation
        // 5. Token Program (readonly) - SPL Token or Token-2022 program
        // Data: empty (discriminator is handled by program)
        let accounts = vec![
            solana_sdk::instruction::AccountMeta::new(wallet_pubkey, true),
            solana_sdk::instruction::AccountMeta::new(ata, false),
            solana_sdk::instruction::AccountMeta::new_readonly(wallet_pubkey, false),
            solana_sdk::instruction::AccountMeta::new_readonly(*mint, false),
            solana_sdk::instruction::AccountMeta::new_readonly(
                solana_sdk::system_program::id(),
                false,
            ),
            solana_sdk::instruction::AccountMeta::new_readonly(token_program, false),
        ];
        
        // Log detailed account information
        log::info!("📋 Executor ATA Instruction Account Details:");
        for (idx, account_meta) in accounts.iter().enumerate() {
            log::info!(
                "   [{}] pubkey={}, is_signer={}, is_writable={}",
                idx,
                account_meta.pubkey,
                account_meta.is_signer,
                account_meta.is_writable
            );
        }
        log::info!("📋 Instruction data length: {} bytes", 0);
        log::info!("📋 Program ID: {}", associated_token_program);
        
        Ok(solana_sdk::instruction::Instruction {
            program_id: associated_token_program,
            accounts,
            data: vec![],
        })
    }

    async fn send_via_jito_with_retry(
        &self,
        tx_builder: &TransactionBuilder,
        jito_client: &Arc<JitoClient>,
    ) -> Result<solana_sdk::signature::Signature> {
        Self::retry_with_backoff(
            || async { self.send_via_jito(tx_builder, jito_client).await },
            |sig| sig == solana_sdk::signature::Signature::default(),
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
        Self::retry_with_backoff(
            || async {
                let blockhash = self.rpc.get_recent_blockhash().await?;
                let mut tx = tx_builder.build(blockhash);
                sign_transaction(&mut tx, &self.wallet)
                    .context("Failed to sign transaction - double signing detected")?;
                send_and_confirm(tx, Arc::clone(&self.rpc)).await
            },
            |_| false,
            "RPC TX",
        ).await
    }

    async fn retry_with_backoff<F, Fut, P>(
        op: F,
        is_invalid: P,
        op_name: &str,
    ) -> Result<solana_sdk::signature::Signature>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<solana_sdk::signature::Signature>>,
        P: Fn(solana_sdk::signature::Signature) -> bool,
    {
        const MAX_TX_RETRIES: u32 = 3;

        for attempt in 1..=MAX_TX_RETRIES {
            match op().await {
                Ok(sig) => {
                    if is_invalid(sig) {
                        if attempt < MAX_TX_RETRIES {
                            log::warn!("Invalid signature on attempt {}, retrying...", attempt);
                            sleep(Duration::from_millis(500 * attempt as u64)).await;
                            continue;
                        }
                        return Err(anyhow::anyhow!("Invalid signature after {} attempts", MAX_TX_RETRIES));
                    }
                    if attempt > 1 {
                        log::info!("{} sent successfully on attempt {}", op_name, attempt);
                    }
                    return Ok(sig);
                }
                Err(e) => {
                    let is_retryable = crate::utils::error_helpers::is_retryable_error(&e);
                    if is_retryable && attempt < MAX_TX_RETRIES {
                        log::warn!("{} failed (attempt {}/{}), retrying: {}", op_name, attempt, MAX_TX_RETRIES, e);
                        sleep(Duration::from_millis(500 * attempt as u64)).await;
                    } else {
                        if attempt >= MAX_TX_RETRIES {
                            log::error!("{} failed after {} attempts: {}", op_name, MAX_TX_RETRIES, e);
                        }
                        return Err(e);
                    }
                }
            }
        }
        Err(anyhow::anyhow!("Failed to send transaction after {} attempts", MAX_TX_RETRIES))
    }
}
