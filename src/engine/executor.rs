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
use anyhow::Result;
use std::sync::Arc;
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::broadcast;
use tokio::time::{sleep, Duration};

struct TxLock {
    locked: Arc<std::sync::RwLock<HashSet<Pubkey>>>,
    lock_times: Arc<std::sync::RwLock<HashMap<Pubkey, Instant>>>,
    timeout_seconds: u64,
}

impl TxLock {
    fn new(timeout_seconds: u64) -> Self {
        TxLock {
            locked: Arc::new(std::sync::RwLock::new(HashSet::new())),
            lock_times: Arc::new(std::sync::RwLock::new(HashMap::new())),
            timeout_seconds,
        }
    }

    /// Start background cleanup task that periodically removes expired locks.
    /// This prevents locks from leaking when no new lock attempts are made.
    fn start_cleanup_task(self: &Arc<Self>) {
        let locked = Arc::clone(&self.locked);
        let lock_times = Arc::clone(&self.lock_times);
        let timeout_seconds = self.timeout_seconds;
        
        tokio::spawn(async move {
            loop {
                // Run cleanup every 10 seconds
                sleep(Duration::from_secs(10)).await;
                
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
        });
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

        // Check if destination_collateral ATA exists, create if missing
        let destination_collateral = get_associated_token_address(
            &wallet_pubkey,
            &opp.collateral_mint,
            Some(&self.config),
        )
        .context("Failed to derive destination collateral ATA")?;
        
        // Check if ATA exists
        let ata_exists = match self.rpc.get_account(&destination_collateral).await {
            Ok(_) => true,
            Err(e) => {
                let error_msg = e.to_string();
                if error_msg.contains("AccountNotFound") || error_msg.contains("account not found") {
                    false
                } else {
                    // Other errors (network issues, etc.) - log but continue
                    log::warn!(
                        "Executor: Failed to check destination_collateral ATA ({}): {}",
                        destination_collateral,
                        e
                    );
                    false // Assume missing and try to create
                }
            }
        };
        
        let blockhash = self.rpc.get_recent_blockhash().await?;
        let mut tx_builder = TransactionBuilder::new(wallet_pubkey);
        tx_builder.add_compute_budget(200_000, 1_000);
        
        // Add create_ata instruction if ATA doesn't exist
        if !ata_exists {
            log::info!(
                "Executor: destination_collateral ATA not found ({}), adding create_ata instruction",
                destination_collateral
            );
            let create_ata_ix = self.create_ata_instruction(&opp.collateral_mint)?;
            tx_builder.add_instruction(create_ata_ix);
        }
        
        tx_builder.add_instruction(liq_ix);
        let mut tx = tx_builder.build(blockhash);

        sign_transaction(&mut tx, &self.wallet);

        let signature = if self.config.dry_run {
            log::info!("DRY RUN: Would send transaction (not sending to blockchain)");
            return Err(anyhow::anyhow!("DRY_RUN mode: Transaction not sent to blockchain"));
        } else if let Some(ref jito) = self.jito_client {
            // Send via Jito bundle for MEV protection
            self.send_via_jito(tx, jito).await?
        } else {
            // Fallback to standard RPC (vulnerable to front-running)
            send_and_confirm(tx, Arc::clone(&self.rpc)).await?
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

    /// Send transaction via Jito bundle for MEV protection
    async fn send_via_jito(
        &self,
        tx: Transaction,
        jito_client: &Arc<JitoClient>,
    ) -> Result<solana_sdk::signature::Signature> {
        use solana_sdk::signature::Signer;
        
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
        
        // Re-sign main transaction with new blockhash
        let mut tx_with_new_blockhash = tx;
        tx_with_new_blockhash.message.recent_blockhash = blockhash;
        sign_transaction(&mut tx_with_new_blockhash, &self.wallet);
        
        // Add main liquidation transaction
        bundle.add_transaction(tx_with_new_blockhash);
        
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
        if let Some(main_tx) = bundle.transactions().get(1) {
            Ok(main_tx.signatures[0])
        } else {
            Err(anyhow::anyhow!("Bundle missing main transaction"))
        }
    }
}
