use crate::balance_reservation::BalanceReservation;
use crate::config::Config;
use crate::event::Event;
use crate::event_bus::EventBus;
use crate::performance::PerformanceTracker;
use crate::protocol::Protocol;
use crate::solana_client::{self, SolanaClient};
use crate::tx_lock::TxLock;
use crate::wallet::WalletManager;
use anyhow::Result;
use solana_sdk::pubkey::Pubkey;
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
    balance_reservation: Arc<BalanceReservation>,
) -> Result<()> {
    let tx_lock = Arc::new(TxLock::new(config.tx_lock_timeout_seconds));

    let max_retries = config.max_retries;
    let initial_retry_delay_ms = config.initial_retry_delay_ms;

    if !config.dry_run {
        log::warn!("PRODUCTION MODE: Real transactions will be sent!");
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

                let _unlock_guard = UnlockGuard {
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

                    for attempt in 0..=max_retries {
                        if let Ok(debt_mint) = crate::utils::parse_pubkey(&opportunity.target_debt_mint) {
                            use crate::wallet::WalletBalanceChecker;
                            let balance_checker = WalletBalanceChecker::new(
                                *wallet.pubkey(),
                                Arc::clone(&rpc_client),
                                Some(config.clone()),
                            );
                            if let Ok(current_balance) = balance_checker.get_token_balance(&debt_mint).await {
                                let reserved = balance_reservation.get_reserved(&debt_mint).await;
                                let available = current_balance.saturating_sub(reserved);
                                
                                if available < opportunity.max_liquidatable_amount {
                                    let error_msg = format!(
                                        "Insufficient balance: required={}, available={}, reserved={}, current={}",
                                        opportunity.max_liquidatable_amount,
                                        available,
                                        reserved,
                                        current_balance
                                    );
                                    if attempt == max_retries {
                                        last_error = Some(anyhow::anyhow!(error_msg));
                                        break;
                                    }
                                    let retry_delay = Duration::from_millis(initial_retry_delay_ms * (attempt + 1) as u64);
                                    sleep(retry_delay).await;
                                    continue;
                                }
                            }
                        }
                        
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
                                log::info!("Liquidation transaction sent: {}", sig);

                                if let Ok(debt_mint) = crate::utils::parse_pubkey(&opportunity.target_debt_mint) {
                                    balance_reservation.release(&debt_mint, opportunity.max_liquidatable_amount).await;
                                }

                                let opportunity_id = opportunity.account_position.account_address.clone();
                                if let Some(latency) = performance_tracker.record_tx_send(opportunity_id).await {
                                    log::info!(
                                        "Transaction sent: {} (profit=${:.2}, attempt={}, latency={}ms)",
                                        sig,
                                        opportunity.estimated_profit_usd,
                                        attempt + 1,
                                        latency.as_millis()
                                    );
                                }
                                break;
                            }
                            Err(e) => {
                                last_error = Some(e);
                                if attempt < max_retries {
                                    let delay_ms = initial_retry_delay_ms * (1 << attempt);
                                    log::warn!(
                                        "Liquidation attempt {} failed: {}. Retrying in {}ms...",
                                        attempt + 1,
                                        last_error.as_ref().unwrap(),
                                        delay_ms
                                    );
                                    sleep(Duration::from_millis(delay_ms)).await;
                                }
                            }
                        }
                    }

                    if !success {
                        if let Ok(debt_mint) = crate::utils::parse_pubkey(&opportunity.target_debt_mint) {
                            balance_reservation.release(&debt_mint, opportunity.max_liquidatable_amount).await;
                        }
                        if let Some(ref err) = last_error {
                            log::error!("All {} attempts failed for account {}: {}", max_retries + 1, account_address, err);
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
