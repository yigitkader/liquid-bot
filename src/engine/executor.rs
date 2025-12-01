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
use tokio::sync::broadcast;

struct TxLock {
    locked: std::sync::Arc<std::sync::RwLock<std::collections::HashSet<Pubkey>>>,
    timeout_seconds: u64,
}

impl TxLock {
    fn new(timeout_seconds: u64) -> Self {
        TxLock {
            locked: Arc::new(std::sync::RwLock::new(std::collections::HashSet::new())),
            timeout_seconds,
        }
    }

    fn try_lock(&self, address: &Pubkey) -> Result<TxLockGuard> {
        let mut locked = self.locked.write().unwrap();
        if locked.contains(address) {
            return Err(anyhow::anyhow!("Account already locked"));
        }
        locked.insert(*address);
        Ok(TxLockGuard {
            locked: Arc::clone(&self.locked),
            address: *address,
        })
    }
}

struct TxLockGuard {
    locked: Arc<std::sync::RwLock<std::collections::HashSet<Pubkey>>>,
    address: Pubkey,
}

impl Drop for TxLockGuard {
    fn drop(&mut self) {
        let mut locked = self.locked.write().unwrap();
        locked.remove(&self.address);
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
                    let _guard = match self.tx_lock.try_lock(&opportunity.position.address) {
                        Ok(guard) => guard,
                        Err(_) => {
                            log::warn!("Account {} already locked, skipping", opportunity.position.address);
                            continue;
                        }
                    };

                    match self.execute(opportunity).await {
                        Ok(signature) => {
                            log::info!("Transaction sent: {}", signature);
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
