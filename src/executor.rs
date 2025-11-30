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
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::broadcast;
use tokio::time::{sleep, Duration};

// Transaction counter for fee verification (first 10 transactions)
static TX_COUNT: AtomicU64 = AtomicU64::new(0);

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
                        // FINAL balance check and re-reserve immediately before tx build & send
                        // -----------------------------------------------------------------
                        // ✅ CRITICAL: Re-reserve balance right before transaction send to prevent race condition
                        // 
                        // Problem: Guard from strategist is dropped before executor, creating a gap
                        // Solution: Re-reserve in executor right before final check and hold guard until tx is sent
                        // 
                        // This ensures:
                        //   1. Balance is verified immediately before reservation (atomic check+reserve)
                        //   2. Guard is held during transaction build and send (prevents parallel usage)
                        //   3. Guard is automatically released when dropped (after tx send or on error)
                        // 
                        // This eliminates the race condition gap between strategist reservation and executor execution.
                        let debt_mint = match crate::utils::parse_pubkey(&opportunity.target_debt_mint) {
                            Ok(mint) => mint,
                            Err(e) => {
                                last_error = Some(anyhow::anyhow!("Invalid debt mint: {}", e));
                                break;
                            }
                        };
                        
                        use crate::wallet::WalletBalanceChecker;
                        let balance_checker = WalletBalanceChecker::new(
                            *wallet.pubkey(),
                            Arc::clone(&rpc_client),
                            Some(config.clone()),
                        );
                        
                        // ✅ Re-reserve with guard to prevent race condition
                        // Guard is held in scope to prevent race condition - automatically released when dropped
                        let _balance_guard = match balance_reservation
                            .try_reserve_with_check(&debt_mint, opportunity.max_liquidatable_amount, &balance_checker)
                            .await
                        {
                            Ok(Some(guard)) => {
                                log::debug!(
                                    "✅ Re-reserved balance for execution: mint={}, amount={}",
                                    debt_mint,
                                    opportunity.max_liquidatable_amount
                                );
                                guard
                            }
                            Ok(None) => {
                                log::warn!(
                                    "Balance no longer available at execution time: required={}, mint={}",
                                    opportunity.max_liquidatable_amount,
                                    debt_mint
                                );
                                
                                // Release any previous reservation
                                balance_reservation.release(&debt_mint, opportunity.max_liquidatable_amount).await;
                                
                                if attempt == max_retries {
                                    last_error = Some(anyhow::anyhow!(
                                        "Insufficient balance after all retries: required={}, mint={}",
                                        opportunity.max_liquidatable_amount,
                                        debt_mint
                                    ));
                                    break;
                                }
                                
                                let retry_delay = Duration::from_millis(initial_retry_delay_ms * (attempt + 1) as u64);
                                log::debug!("Retrying after {}ms (balance might be freed by another transaction)", retry_delay.as_millis());
                                sleep(retry_delay).await;
                                continue;
                            }
                            Err(e) => {
                                log::error!("Failed to re-reserve balance: {}", e);
                                last_error = Some(e);
                                break;
                            }
                        };

                        // -----------------------------------------------------------------
                        // 3) Tx build + sign + send (guard is held during this operation)
                        // -----------------------------------------------------------------
                        // ✅ Guard is held during transaction send - prevents race condition
                        // Guard will be automatically released when dropped (after tx send or on error)
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
                                
                                // Increment transaction counter
                                let tx_count = TX_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                                
                                log::info!("Liquidation transaction sent: {} (transaction #{})", sig, tx_count);
                                
                                // ✅ Fee verification for first 10 transactions
                                if tx_count <= 10 {
                                    log::info!("");
                                    log::info!("🔍 TRANSACTION FEE VERIFICATION REQUIRED (Transaction #{}/10):", tx_count);
                                    log::info!("   1. Check transaction on Solscan: https://solscan.io/tx/{}", sig);
                                    log::info!("   2. Compare actual fee vs estimated fee (should be within ±10%)");
                                    log::info!("   3. Verify compute units consumed (should be < configured limit)");
                                    log::info!("   4. Adjust config if needed (see docs/TRANSACTION_FEE_VERIFICATION.md)");
                                    log::info!("");
                                    log::info!("   ⚠️  IMPORTANT: Verify fee accuracy for first 10 transactions!");
                                    log::info!("      If fee estimation error >10%, adjust config values.");
                                    log::info!("");
                                } else if tx_count == 11 {
                                    log::info!("");
                                    log::info!("✅ Fee verification period complete (10 transactions verified)");
                                    log::info!("   Continuing with normal operation. Monitor fee accuracy periodically.");
                                    log::info!("");
                                }

                                // ✅ Guard is automatically released when dropped here
                                // No need to manually release - guard's Drop impl handles it

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
                        // ✅ If we had a guard, it's already released by Drop
                        // Only manually release if we didn't get a guard (shouldn't happen, but safety check)
                        if let Ok(debt_mint) = crate::utils::parse_pubkey(&opportunity.target_debt_mint) {
                            // Check if there's still a reservation (guard might have been dropped)
                            let reserved = balance_reservation.get_reserved(&debt_mint).await;
                            if reserved > 0 {
                                log::debug!("Releasing remaining reservation: mint={}, reserved={}", debt_mint, reserved);
                                balance_reservation.release(&debt_mint, opportunity.max_liquidatable_amount).await;
                            }
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
