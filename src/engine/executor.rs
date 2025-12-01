use crate::core::events::{Event, EventBus};
use crate::core::types::Opportunity;
use crate::blockchain::rpc_client::RpcClient;
use crate::blockchain::transaction::{TransactionBuilder, sign_transaction, send_and_confirm};
use crate::protocol::Protocol;
use crate::strategy::balance_manager::BalanceManager;
use crate::core::config::Config;
use solana_sdk::signature::Keypair;
use solana_sdk::pubkey::Pubkey;
use anyhow::Result;
use std::sync::Arc;
use std::collections::{HashMap, HashSet};
use std::time::Instant;
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
        
        Executor {
            event_bus,
            rpc,
            wallet,
            protocol,
            balance_manager,
            tx_lock,
            config,
        }
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
                        }
                        Err(e) => {
                            log::error!("Failed to execute liquidation: {}", e);
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
        let wallet_pubkey = self.wallet.pubkey();
        let liq_ix = self.protocol
            .build_liquidation_ix(&opp, &wallet_pubkey, Some(Arc::clone(&self.rpc)))
            .await?;

        let blockhash = self.rpc.get_recent_blockhash().await?;
        let mut tx_builder = TransactionBuilder::new(wallet_pubkey);
        tx_builder
            .add_compute_budget(200_000, 1_000)
            .add_instruction(liq_ix);
        let mut tx = tx_builder.build(blockhash);

        sign_transaction(&mut tx, &self.wallet);

        let signature = if self.config.dry_run {
            log::info!("DRY RUN: Would send transaction (not sending to blockchain)");
            return Err(anyhow::anyhow!("DRY_RUN mode: Transaction not sent to blockchain"));
        } else {
            send_and_confirm(tx, Arc::clone(&self.rpc)).await?
        };

        self.balance_manager.release(&opp.debt_mint, opp.max_liquidatable).await;

        self.event_bus.publish(Event::TransactionSent {
            signature: signature.to_string(),
        })?;

        Ok(signature)
    }
}
