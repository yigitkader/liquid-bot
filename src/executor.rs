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

                // UnlockGuard automatically unlocks the account when dropped
                // This ensures the lock is released even if an error occurs
                let _unlock_guard = UnlockGuard {
                    tx_lock: Arc::clone(&tx_lock),
                    account_address: account_address.clone(),
                };

                if config.dry_run {
                    // ⚠️ VERIFICATION: Log transaction fee breakdown in dry-run mode
                    // This helps verify fee calculation against real mainnet transactions
                    
                    // Calculate actual transaction fee for verification
                    use crate::math::calculate_transaction_fee_usd;
                    let sol_price_usd = config.sol_price_fallback_usd;
                    let estimated_tx_fee_usd = calculate_transaction_fee_usd(
                        config.liquidation_compute_units,
                        config.priority_fee_per_cu,
                        config.base_transaction_fee_lamports,
                        config.oracle_read_fee_lamports,
                        config.oracle_accounts_read,
                        sol_price_usd,
                    );
                    
                    let priority_fee_lamports = (config.liquidation_compute_units as u64) * config.priority_fee_per_cu / 1_000_000;
                    let total_fee_lamports = config.base_transaction_fee_lamports + priority_fee_lamports;
                    let total_fee_sol = total_fee_lamports as f64 / 1_000_000_000.0;
                    
                    log::info!(
                        "🔍 DRY RUN: Would execute liquidation for account {} (profit=${:.2})",
                        account_address,
                        opportunity.estimated_profit_usd
                    );
                    log::info!(
                        "   💰 Estimated Transaction Fee: ${:.6} USD \
                         (Base: {} lamports, Priority: {} lamports, Total: {} lamports = {:.9} SOL) \
                         [⚠️ VERIFICATION: Compare with real mainnet tx fees from Solana Explorer]",
                        estimated_tx_fee_usd,
                        config.base_transaction_fee_lamports,
                        priority_fee_lamports,
                        total_fee_lamports,
                        total_fee_sol
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
                        // ⚠️ RACE CONDITION PROTECTION: Double-check balance before sending transaction
                        // Even though balance was reserved atomically, there's a gap between:
                        // 1. Balance check + reservation (in strategist)
                        // 2. Transaction send (here in executor)
                        // 
                        // During this gap, another transaction could have consumed the balance.
                        // This final check prevents sending a transaction that will fail due to insufficient balance.
                        if let Ok(debt_mint) = Pubkey::try_from(opportunity.target_debt_mint.as_str()) {
                            use crate::wallet::WalletBalanceChecker;
                            let balance_checker = WalletBalanceChecker::new(
                                *wallet.pubkey(),
                                Arc::clone(&rpc_client),
                                Some(config.clone()),
                            );
                            match balance_checker.get_token_balance(&debt_mint).await {
                                Ok(current_balance) => {
                                    let reserved = balance_reservation.get_reserved(&debt_mint).await;
                                    let available = current_balance.saturating_sub(reserved);
                                    
                                    if available < opportunity.max_liquidatable_amount {
                                        let error_msg = format!(
                                            "Insufficient balance for liquidation: required={}, available={}, reserved={}, current={}",
                                            opportunity.max_liquidatable_amount,
                                            available,
                                            reserved,
                                            current_balance
                                        );
                                        log::warn!("⚠️  {} (attempt {}/{})", error_msg, attempt + 1, max_retries + 1);
                                        
                                        // If this is the last attempt, fail with this error
                                        if attempt == max_retries {
                                            last_error = Some(anyhow::anyhow!(error_msg));
                                            break;
                                        }
                                        
                                        // Otherwise, wait a bit and retry (balance might be freed by another transaction)
                                        let retry_delay = Duration::from_millis(
                                            initial_retry_delay_ms * (attempt + 1) as u64
                                        );
                                        log::debug!("Retrying after {}ms...", retry_delay.as_millis());
                                        sleep(retry_delay).await;
                                        continue;
                                    }
                                    
                                    log::debug!(
                                        "✅ Balance check passed: required={}, available={}, reserved={}, current={}",
                                        opportunity.max_liquidatable_amount,
                                        available,
                                        reserved,
                                        current_balance
                                    );
                                }
                                Err(e) => {
                                    log::warn!(
                                        "⚠️  Failed to check balance before transaction send: {} (attempt {}/{})",
                                        e,
                                        attempt + 1,
                                        max_retries + 1
                                    );
                                    // Continue with transaction send - if balance is insufficient, transaction will fail
                                    // and we'll retry or handle the error
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

                                // ⚠️ VERIFICATION: Log transaction signature for fee verification
                                log::info!(
                                    "✅ Liquidation transaction sent: {}",
                                    sig
                                );
                                log::info!(
                                    "   🔍 VERIFICATION: Check transaction fee on Solana Explorer: \
                                     https://solscan.io/tx/{} \
                                     Compare actual fee with estimated fee from logs",
                                    sig
                                );

                                // Release balance reservation after successful transaction
                                if let Ok(debt_mint) = Pubkey::try_from(opportunity.target_debt_mint.as_str()) {
                                    balance_reservation.release(&debt_mint, opportunity.max_liquidatable_amount).await;
                                    log::debug!(
                                        "Released balance reservation: mint={}, amount={}",
                                        debt_mint,
                                        opportunity.max_liquidatable_amount
                                    );
                                }

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
                                if attempt < max_retries {
                                    let delay_ms = initial_retry_delay_ms * (1 << attempt);
                                    log::warn!(
                                        "Liquidation attempt {} failed for account {}: {}. Retrying in {}ms...",
                                        attempt + 1,
                                        account_address,
                                        last_error.as_ref().expect("last_error should be Some() here"),
                                        delay_ms
                                    );
                                    sleep(Duration::from_millis(delay_ms)).await;
                                } else {
                                    log::error!(
                                        "All {} liquidation attempts failed for account {}: {}",
                                        max_retries + 1,
                                        account_address,
                                        last_error.as_ref().unwrap()
                                    );
                                }
                            }
                        }
                    }

                    // Release balance reservation if transaction failed
                    if !success {
                        if let Ok(debt_mint) = Pubkey::try_from(opportunity.target_debt_mint.as_str()) {
                            balance_reservation.release(&debt_mint, opportunity.max_liquidatable_amount).await;
                            log::debug!(
                                "Released balance reservation after failure: mint={}, amount={}",
                                debt_mint,
                                opportunity.max_liquidatable_amount
                            );
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
