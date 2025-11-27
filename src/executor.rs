use crate::config::Config;
use crate::event::Event;
use crate::event_bus::EventBus;
use crate::performance::PerformanceTracker;
use crate::protocol::Protocol;
use crate::solana_client::{self, SolanaClient};
use crate::tx_lock::TxLock;
use crate::wallet::WalletManager;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::time::{sleep, Duration};

pub async fn run_executor(
    mut receiver: broadcast::Receiver<Event>,
    bus: EventBus,
    config: Config,
    wallet: Arc<WalletManager>,
    protocol: Arc<dyn Protocol>,
    rpc_client: Arc<SolanaClient>,
    performance_tracker: Arc<PerformanceTracker>,
) -> Result<()> {
    let tx_lock = Arc::new(TxLock::new(60));

    const MAX_RETRIES: u32 = 3;
    const INITIAL_RETRY_DELAY_MS: u64 = 1000; // 1 saniye

    if !config.dry_run {
        log::warn!("⚠️  PRODUCTION MODE: Real transactions will be sent!");
        log::warn!("⚠️  Double-checking DRY_RUN=false is intentional...");
    }
    loop {
        match receiver.recv().await {
            Ok(Event::ExecuteLiquidation(opportunity)) => {
                let account_address = opportunity.account_position.account_address.clone();

                if !tx_lock.try_lock(&account_address).await {
                    log::warn!(
                        "Account {} is already being processed, skipping duplicate liquidation",
                        account_address
                    );
                    continue;
                }

                let unlock_guard = UnlockGuard {
                    //todo: why this is not used ?
                    tx_lock: Arc::clone(&tx_lock),
                    account_address: account_address.clone(),
                };

                if config.dry_run {
                    log::info!(
                        "DRY RUN: Would execute liquidation for account {} (profit=${:.2})",
                        account_address,
                        opportunity.estimated_profit_usd
                    );

                    bus.publish(Event::TxResult {
                        opportunity: opportunity.clone(),
                        success: true,
                        signature: Some("DRY_RUN_SIGNATURE".to_string()),
                        error: None,
                    })?;
                } else {
                    let mut last_error = None;
                    let mut success = false;
                    let mut signature = None;

                    for attempt in 0..=MAX_RETRIES {
                        match solana_client::execute_liquidation(
                            &opportunity,
                            &config,
                            wallet.as_ref(),
                            protocol.as_ref(),
                            Arc::clone(&rpc_client),
                        )
                        .await
                        {
                            Ok(sig) => {
                                signature = Some(sig.clone());
                                success = true;

                                let opportunity_id =
                                    opportunity.account_position.account_address.clone();
                                if let Some(latency) =
                                    performance_tracker.record_tx_send(opportunity_id).await
                                {
                                    log::info!(
                                        "Liquidation transaction sent: {} (profit=${:.2}, attempt={}, latency={}ms)",
                                        sig,
                                        opportunity.estimated_profit_usd,
                                        attempt + 1,
                                        latency.as_millis()
                                    );
                                } else {
                                    log::info!(
                                        "Liquidation transaction sent: {} (profit=${:.2}, attempt={})",
                                        sig,
                                        opportunity.estimated_profit_usd,
                                        attempt + 1
                                    );
                                }
                                break;
                            }
                            Err(e) => {
                                last_error = Some(e);
                                if attempt < MAX_RETRIES {
                                    let delay_ms = INITIAL_RETRY_DELAY_MS * (1 << attempt);
                                    log::warn!(
                                        "Liquidation attempt {} failed for account {}: {}. Retrying in {}ms...",
                                        attempt + 1,
                                        account_address,
                                        last_error.as_ref().unwrap(),
                                        delay_ms
                                    );
                                    sleep(Duration::from_millis(delay_ms)).await;
                                } else {
                                    log::error!(
                                        "All {} liquidation attempts failed for account {}: {}",
                                        MAX_RETRIES + 1,
                                        account_address,
                                        last_error.as_ref().unwrap()
                                    );
                                }
                            }
                        }
                    }

                    bus.publish(Event::TxResult {
                        opportunity: opportunity.clone(),
                        success,
                        signature,
                        error: last_error.map(|e| e.to_string()),
                    })?;
                }
            }
            Ok(_) => {
                // Diğer event'leri ignore et
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

struct UnlockGuard {
    tx_lock: Arc<TxLock>,
    account_address: String,
}

impl Drop for UnlockGuard {
    fn drop(&mut self) {
        let tx_lock = Arc::clone(&self.tx_lock);
        let account_address = self.account_address.clone();
        tokio::spawn(async move {
            tx_lock.unlock(&account_address).await;
        });
    }
}
