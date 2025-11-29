use crate::balance_reservation::BalanceReservation;
use crate::config::Config;
use crate::event::Event;
use crate::event_bus::EventBus;
use crate::performance::PerformanceTracker;
use crate::protocol::Protocol;
use crate::solana_client::{self, SolanaClient};
use crate::slippage_calibration::SlippageCalibrator;
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
    balance_reservation: Arc<BalanceReservation>,
) -> Result<()> {
    let tx_lock = Arc::new(TxLock::new(config.tx_lock_timeout_seconds));

    // Initialize slippage calibrator if calibration file is configured
    let slippage_calibrator = if let Some(calibration_file) = &config.slippage_calibration_file {
        match SlippageCalibrator::new(
            calibration_file.clone(),
            config.slippage_size_small_threshold_usd,
            config.slippage_size_large_threshold_usd,
            config.max_slippage_bps,
            config.slippage_min_measurements_per_category,
        ) {
            Ok(calibrator) => {
                log::info!("Slippage calibration enabled: {}", calibration_file);
                Some(Arc::new(calibrator))
            }
            Err(e) => {
                log::warn!("Failed to initialize slippage calibrator: {}. Calibration disabled.", e);
                None
            }
        }
    } else {
        None
    };

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
                    
                    log::info!(
                        "DRY RUN: Would execute liquidation for account {} (profit=${:.2})",
                        account_address,
                        opportunity.estimated_profit_usd
                    );
                    log::info!("   Estimated transaction fee: ${:.6} USD", estimated_tx_fee_usd);
                    log::info!("");
                    log::info!("   ⚠️  After first real liquidation, verify fee on Solscan:");
                    log::info!("      - Compare actual vs estimated fee (should be within ±10%)");
                    log::info!("      - See docs/TRANSACTION_FEE_VERIFICATION.md for details");
                    log::info!("");
                    
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
                        // -----------------------------------------------------------------
                        // 1) Balance check (early reject / retry)
                        // -----------------------------------------------------------------
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
                                    log::warn!(
                                        "Balance insufficient at execution time: required={}, available={}, reserved={}, current={}",
                                        opportunity.max_liquidatable_amount,
                                        available,
                                        reserved,
                                        current_balance
                                    );
                                    
                                    balance_reservation.release(&debt_mint, opportunity.max_liquidatable_amount).await;
                                    log::debug!("Released reservation due to insufficient balance");
                                    
                                    if attempt == max_retries {
                                        last_error = Some(anyhow::anyhow!(
                                            "Insufficient balance after all retries: required={}, available={}",
                                            opportunity.max_liquidatable_amount,
                                            available
                                        ));
                                        break;
                                    }
                                    
                                    let retry_delay = Duration::from_millis(initial_retry_delay_ms * (attempt + 1) as u64);
                                    log::debug!("Retrying after {}ms (balance might be freed by another transaction)", retry_delay.as_millis());
                                    sleep(retry_delay).await;
                                    continue;
                                }
                            }
                        }
                        
                        // -----------------------------------------------------------------
                        // 2) FINAL balance re-check immediately before tx build & send
                        // -----------------------------------------------------------------
                        // ✅ CRITICAL: Final balance check to prevent race condition
                        // 
                        // Three-Layer Race Condition Protection:
                        // 
                        // Layer 1: Reservation Layer (balance_reservation.rs::try_reserve_with_check)
                        //   - Prevents parallel opportunities from over-reserving the same balance
                        //   - Uses double-check pattern: initial check + fresh check inside lock
                        //   - Minimizes gap between balance check and reservation
                        // 
                        // Layer 2: Double-Check Layer (balance_reservation.rs::try_reserve_with_check)
                        //   - Re-verifies balance immediately before reservation (inside lock)
                        //   - Catches any balance changes that occurred during lock acquisition
                        //   - Uses fresh balance for reservation calculation
                        // 
                        // Layer 3: Final Check Layer (this code - executor.rs)
                        //   - Performs final balance check immediately before transaction send
                        //   - Catches any balance changes that occurred after reservation
                        //   - Prevents sending transactions that will fail (saves fees)
                        //   - Releases reservation if balance is insufficient
                        // 
                        // This three-layer protection ensures:
                        //   1. Parallel opportunities don't over-reserve (Layer 1)
                        //   2. Balance is verified immediately before reservation (Layer 2)
                        //   3. Transactions that would fail are not sent (Layer 3)
                        //   4. Wasted transaction fees are prevented (Layer 3)
                        // 
                        // See balance_reservation.rs for more details on Layers 1 and 2.
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
                                    log::warn!(
                                        "Final balance check failed right before tx send: required={}, available={}, reserved={}, current={}",
                                        opportunity.max_liquidatable_amount,
                                        available,
                                        reserved,
                                        current_balance
                                    );
                                    log::warn!(
                                        "   This can happen if balance was consumed between reservation and final check. \
                                         Reservation is released to allow other opportunities to proceed."
                                    );

                                    balance_reservation
                                        .release(&debt_mint, opportunity.max_liquidatable_amount)
                                        .await;
                                    log::debug!("Released reservation due to insufficient balance at final check");

                                    last_error = Some(anyhow::anyhow!(
                                        "Insufficient balance at final check: required={}, available={} \
                                         (balance may have been consumed by another transaction between reservation and final check)",
                                        opportunity.max_liquidatable_amount,
                                        available
                                    ));

                                    // Bu attempt'ten ve tüm retry'lardan vazgeç
                                    break;
                                } else {
                                    log::debug!(
                                        "✅ Final balance check passed: required={}, available={}, reserved={}, current={}",
                                        opportunity.max_liquidatable_amount,
                                        available,
                                        reserved,
                                        current_balance
                                    );
                                }
                            }
                        }

                        // -----------------------------------------------------------------
                        // 3) Tx build + sign + send
                        // -----------------------------------------------------------------
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
                                
                                log::info!("");
                                log::info!("🔍 TRANSACTION FEE VERIFICATION REQUIRED:");
                                log::info!("   1. Check transaction on Solscan: https://solscan.io/tx/{}", sig);
                                log::info!("   2. Compare actual fee vs estimated fee (should be within ±10%)");
                                log::info!("   3. Verify compute units consumed (should be < configured limit)");
                                log::info!("   4. Adjust config if needed (see docs/TRANSACTION_FEE_VERIFICATION.md)");
                                log::info!("");

                                if let Ok(debt_mint) = crate::utils::parse_pubkey(&opportunity.target_debt_mint) {
                                    balance_reservation.release(&debt_mint, opportunity.max_liquidatable_amount).await;
                                }

                                // ✅ Record slippage measurement for calibration
                                if let Some(ref calibrator) = slippage_calibrator {
                                    // Calculate trade size in USD (seizable collateral value)
                                    let trade_size_usd = opportunity.seizable_collateral as f64 / 1_000_000.0; // Assuming 6 decimals
                                    // Estimate slippage from opportunity (this is what we predicted)
                                    // Note: We need to extract estimated slippage from the opportunity
                                    // For now, we'll use a placeholder - actual implementation would extract from opportunity
                                    let estimated_slippage_bps = 50; // Placeholder - should be extracted from opportunity
                                    
                                    // Record measurement (actual slippage will be calculated later from transaction logs)
                                    if let Err(e) = calibrator.record_measurement(
                                        sig.to_string(),
                                        trade_size_usd,
                                        estimated_slippage_bps,
                                        None, // Actual slippage not available yet - will be calculated later
                                    ).await {
                                        log::warn!("Failed to record slippage measurement: {}", e);
                                    } else {
                                        log::debug!("Recorded slippage measurement for calibration: sig={}, size=${:.2}", sig, trade_size_usd);
                                    }
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
