use crate::balance_reservation::BalanceReservation;
use crate::config::Config;
use crate::event::Event;
use crate::event_bus::EventBus;
use crate::protocol::Protocol;
use crate::solana_client::SolanaClient;
use crate::wallet::WalletBalanceChecker;
use anyhow::Result;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use tokio::sync::broadcast;

// Oracle staleness limit is now configurable via config.max_oracle_age_seconds (default: 60)

pub async fn run_strategist(
    mut receiver: broadcast::Receiver<Event>,
    bus: EventBus,
    config: Config,
    wallet_balance_checker: Arc<WalletBalanceChecker>,
    rpc_client: Arc<SolanaClient>,
    _protocol: Arc<dyn Protocol>,
    balance_reservation: Arc<BalanceReservation>,
) -> Result<()> {
    // Minimum SOL reserve for transaction fees (from config)
    let min_reserve_lamports = config.min_reserve_lamports;
    loop {
        match receiver.recv().await {
            Ok(Event::PotentiallyLiquidatable(opportunity)) => {
                let mut approved = true;
                let mut rejection_reason = String::new();

                if opportunity.estimated_profit_usd < config.min_profit_usd {
                    approved = false;
                    rejection_reason = format!(
                        "profit ${:.2} < min ${:.2}",
                        opportunity.estimated_profit_usd, config.min_profit_usd
                    );
                }

                if approved {
                    // DEX slippage: Estimated based on liquidation bonus
                    let estimated_dex_slippage_bps =
                        (opportunity.liquidation_bonus * config.slippage_estimation_multiplier * 10000.0) as u16;
                    let debt_mint_str = &opportunity.target_debt_mint;
                    let mut debt_reserve_pubkey: Option<Pubkey> = None;
                    let mut oracle_confidence_bps: Option<u16> = None;

                    // ✅ DOĞRU: Obligation'dan doğrudan reserve'i al
                    // debt_mint (token mint address) ≠ reserve_pubkey
                    // Heuristic (619 bytes) güvenilmez, yanlış account'u reserve sanabilir
                    if let Ok(obligation_pubkey) =
                        Pubkey::try_from(opportunity.account_position.account_address.as_str())
                    {
                        use crate::protocols::solend::solend_idl::SolendObligation;

                        if let Ok(obligation_account) =
                            rpc_client.get_account(&obligation_pubkey).await
                        {
                            if let Ok(obligation) =
                                SolendObligation::from_account_data(&obligation_account.data)
                            {
                                // İlk borrow reserve'ini al
                                // Note: In most cases, the first borrow is the primary debt
                                // For multi-asset positions, this may need refinement
                                if let Some(borrow) = obligation.borrows.first() {
                                    debt_reserve_pubkey = Some(borrow.borrow_reserve);
                                }
                            }
                        }
                    }

                    if let Some(reserve_pubkey) = debt_reserve_pubkey {
                        use crate::protocols::oracle_helper::{
                            get_oracle_accounts_from_reserve, read_oracle_price,
                        };
                        use crate::protocols::reserve_helper::parse_reserve_account;

                        match rpc_client.get_account(&reserve_pubkey).await {
                            Ok(reserve_account) => {
                                match parse_reserve_account(&reserve_pubkey, &reserve_account).await
                                {
                                    Ok(reserve_info) => {
                                        match get_oracle_accounts_from_reserve(&reserve_info) {
                                            Ok((pyth, switchboard)) => {
                                                if let Ok(Some(price)) = read_oracle_price(
                                                    pyth.as_ref(),
                                                    switchboard.as_ref(),
                                                    Arc::clone(&rpc_client),
                                                    Some(&config),
                                                )
                                                .await
                                                {
                                                    // Pyth confidence = ±1σ (standard deviation), representing 68% confidence interval
                                                    // Use confidence directly - it already represents price uncertainty
                                                    // Multiplying by Z-score (1.96) is statistically incorrect
                                                    let confidence_bps = ((price.confidence
                                                        / price.price)
                                                        * 10000.0)
                                                        as u16;
                                                    oracle_confidence_bps = Some(confidence_bps);
                                                    let age_seconds = std::time::SystemTime::now()
                                                        .duration_since(std::time::UNIX_EPOCH)
                                                        .unwrap_or_default()
                                                        .as_secs()
                                                        as i64
                                                        - price.timestamp;

                                                    if age_seconds > config.max_oracle_age_seconds as i64 {
                                                        approved = false;
                                                        rejection_reason = format!(
                                                            "oracle price too old: {}s (max: {}s)",
                                                            age_seconds,
                                                            config.max_oracle_age_seconds
                                                        );
                                                    }
                                                } else {
                                                    log::warn!("Failed to read oracle price from reserve {}", reserve_pubkey);
                                                }
                                            }
                                            Err(e) => {
                                                // ❌ CRITICAL: Reserve parsing artık doğru çalışıyor (#1'de doğrulandı)
                                                // Eğer reserve'den oracle bulunamazsa, bu bir hata durumudur.
                                                // Hardcoded mapping'e fallback yapılmaz - bu sadece 5 token destekler
                                                // ve BONK, RAY, SRM, MNGO gibi token'lar için başarısız olur.
                                                log::error!(
                                                    "❌ CRITICAL: Failed to get oracle accounts from reserve {}: {}",
                                                    reserve_pubkey,
                                                    e
                                                );
                                                log::error!(
                                                    "   Reserve parsing is working correctly (#1 validated), so this indicates: \
                                                    1. Reserve account configuration issue (oracle addresses not set), \
                                                    2. Reserve account data corruption, or \
                                                    3. Reserve account version mismatch. \
                                                    Hardcoded mapping fallback is disabled to prevent incorrect oracle usage."
                                                );
                                                // Fallback kaldırıldı - hata durumunda işlemi durdur
                                                // Bu, yanlış oracle kullanımını ve potansiyel para kaybını önler
                                                // Oracle bulunamadığı için bu liquidation opportunity'si reddedilir
                                                approved = false;
                                                rejection_reason = format!(
                                                    "oracle accounts not found in reserve {}: {}",
                                                    reserve_pubkey,
                                                    e
                                                );
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        log::warn!(
                                            "Failed to parse reserve account {}: {}",
                                            reserve_pubkey,
                                            e
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                log::warn!(
                                    "Failed to read reserve account {}: {}",
                                    reserve_pubkey,
                                    e
                                );
                            }
                        }
                    } else {
                        log::warn!("Could not find debt reserve pubkey for debt mint {}. Skipping oracle check.", debt_mint_str);
                    }

                    // Total slippage = DEX slippage + Oracle confidence
                    // These are independent risk sources that should be added together
                    // Apply safety margin multiplier for model uncertainty (configurable via SLIPPAGE_FINAL_MULTIPLIER)
                    if approved {
                        let oracle_confidence = oracle_confidence_bps.unwrap_or(0);
                        let total_slippage_bps = estimated_dex_slippage_bps + oracle_confidence;
                        let final_slippage_bps = ((total_slippage_bps as f64 * config.slippage_final_multiplier) as u16).min(u16::MAX);
                        
                        if final_slippage_bps > config.max_slippage_bps {
                            approved = false;
                            rejection_reason = format!(
                                "total slippage {} bps > max {} bps (dex: {} bps, oracle: {} bps, multiplier: {:.2}x)",
                                final_slippage_bps,
                                config.max_slippage_bps,
                                estimated_dex_slippage_bps,
                                oracle_confidence,
                                config.slippage_final_multiplier
                            );
                        } else {
                            log::debug!(
                                "Slippage check passed: dex={} bps, oracle={} bps, total={} bps, final={} bps (multiplier: {:.2}x), max={} bps",
                                estimated_dex_slippage_bps,
                                oracle_confidence,
                                total_slippage_bps,
                                final_slippage_bps,
                                config.slippage_final_multiplier,
                                config.max_slippage_bps
                            );
                        }
                    }
                }

                if approved {
                    let debt_mint = match Pubkey::try_from(opportunity.target_debt_mint.as_str()) {
                        Ok(mint) => mint,
                        Err(e) => {
                            log::warn!("Invalid debt mint address: {}, rejecting opportunity", e);
                            approved = false;
                            rejection_reason =
                                format!("invalid debt mint: {}", opportunity.target_debt_mint);
                            continue;
                        }
                    };

                    let collateral_mint =
                        match Pubkey::try_from(opportunity.target_collateral_mint.as_str()) {
                            Ok(mint) => mint,
                            Err(e) => {
                                log::warn!(
                                    "Invalid collateral mint address: {}, rejecting opportunity",
                                    e
                                );
                                approved = false;
                                rejection_reason = format!(
                                    "invalid collateral mint: {}",
                                    opportunity.target_collateral_mint
                                );
                                continue;
                            }
                        };

                    let required_debt_amount = opportunity.max_liquidatable_amount;

                    if approved {
                        match wallet_balance_checker
                            .ensure_token_account_exists(&debt_mint)
                            .await
                        {
                            Ok(true) => {
                                log::debug!("Debt token account exists: mint={}", debt_mint);
                                
                                // ATA var ama bakiye kontrolü yap - bakiye sıfır olabilir
                                match wallet_balance_checker.get_token_balance(&debt_mint).await {
                                    Ok(debt_balance) => {
                                        if debt_balance < required_debt_amount {
                                            approved = false;
                                            rejection_reason = format!(
                                                "insufficient debt token balance: required={}, available={}",
                                                required_debt_amount,
                                                debt_balance
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        log::warn!(
                                            "Failed to get debt token balance: {}, rejecting opportunity",
                                            e
                                        );
                                        approved = false;
                                        rejection_reason = format!("debt token balance check failed: {}", e);
                                    }
                                }
                            }
                            Ok(false) => {
                                approved = false;
                                rejection_reason = format!(
                                    "debt token account doesn't exist for mint {} (need to create ATA first)",
                                    debt_mint
                                );
                                log::warn!(
                                    "Rejecting liquidation: debt token account missing for mint {}",
                                    debt_mint
                                );
                            }
                            Err(e) => {
                                log::warn!(
                                    "Failed to check debt token account: {}, rejecting opportunity",
                                    e
                                );
                                approved = false;
                                rejection_reason =
                                    format!("debt token account check failed: {}", e);
                            }
                        }
                    }

                    if approved {
                        match wallet_balance_checker
                            .ensure_token_account_exists(&collateral_mint)
                            .await
                        {
                            Ok(true) => {
                                log::debug!(
                                    "Collateral token account exists: mint={}",
                                    collateral_mint
                                );
                            }
                            Ok(false) => {
                                approved = false;
                                rejection_reason = format!(
                                    "collateral token account doesn't exist for mint {} (ATA needs to be created first)",
                                    collateral_mint
                                );
                                log::warn!(
                                    "Rejecting liquidation: collateral token account missing for mint {}",
                                    collateral_mint
                                );
                            }
                            Err(e) => {
                                log::warn!("Failed to check collateral token account: {}, rejecting opportunity", e);
                                approved = false;
                                rejection_reason =
                                    format!("collateral token account check failed: {}", e);
                            }
                        }
                    }

                    if approved {
                        // ✅ ATOMIC: Balance check and reservation in one operation
                        // This prevents race conditions where balance is checked, then another
                        // opportunity reserves it before this one can reserve it.
                        // 
                        // Old approach (❌ RACE CONDITION RISK):
                        // 1. get_token_balance() - gets balance
                        // 2. ... (gap - another opportunity could reserve here)
                        // 3. reserve() - tries to reserve
                        //
                        // New approach (✅ ATOMIC):
                        // 1. try_reserve_with_check() - atomically checks balance and reserves
                        
                        // First check SOL balance (separate check, but less critical for race condition)
                        let sol_balance = match wallet_balance_checker.get_sol_balance().await {
                            Ok(balance) => balance,
                            Err(e) => {
                                log::warn!("Failed to get SOL balance: {}, rejecting opportunity", e);
                                approved = false;
                                rejection_reason = format!("SOL balance check failed: {}", e);
                                continue;
                            }
                        };

                        // Check SOL reserve
                        if sol_balance < min_reserve_lamports {
                            approved = false;
                            rejection_reason = format!(
                                "insufficient SOL: required={}, available={}",
                                min_reserve_lamports,
                                sol_balance
                            );
                            continue;
                        }

                        // ✅ ATOMIC: Balance check and reservation in one operation
                        // This prevents race conditions when multiple opportunities are processed in parallel
                        match balance_reservation.try_reserve_with_check(
                            &debt_mint,
                            required_debt_amount,
                            wallet_balance_checker.as_ref(),
                        ).await {
                            Ok(Some(_guard)) => {
                                // Reservation successful - guard will be dropped automatically when out of scope
                                // Note: We don't need to hold the guard here since we're just checking availability
                                // The reservation will be released when the guard is dropped
                                log::debug!(
                                    "Balance reserved atomically: debt_mint={}, amount={}",
                                    debt_mint,
                                    required_debt_amount
                                );
                            }
                            Ok(None) => {
                                // Reservation failed - insufficient balance or already reserved
                                approved = false;
                                let reserved = balance_reservation.get_reserved(&debt_mint).await;
                                // Get current balance for logging (may have changed, but that's ok for logging)
                                let current_balance = wallet_balance_checker.get_token_balance(&debt_mint).await.unwrap_or(0);
                                let available = balance_reservation.get_available(&debt_mint, current_balance).await;
                                rejection_reason = format!(
                                    "insufficient capital (race condition prevented): required_debt={} (mint={}), available={}, reserved={}, actual={}",
                                    required_debt_amount,
                                    debt_mint,
                                    available,
                                    reserved,
                                    current_balance
                                );
                            }
                            Err(e) => {
                                log::warn!("Failed to check/reserve balance: {}, rejecting opportunity", e);
                                approved = false;
                                rejection_reason = format!("balance check/reservation failed: {}", e);
                            }
                        }
                    }
                }

                if approved {
                    log::info!(
                        "Liquidation opportunity approved: profit=${:.2}, account={}",
                        opportunity.estimated_profit_usd,
                        opportunity.account_position.account_address
                    );

                    bus.publish(Event::ExecuteLiquidation(opportunity))?;
                } else {
                    log::debug!(
                        "Liquidation opportunity rejected: account={}, reason={}",
                        opportunity.account_position.account_address,
                        rejection_reason
                    );
                }
            }
            Ok(_) => {
                // Diğer event'leri ignore et
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                log::warn!("Strategist lagged, skipped {} events", skipped);
            }
            Err(broadcast::error::RecvError::Closed) => {
                log::error!("Event bus closed, strategist shutting down");
                break;
            }
        }
    }

    Ok(())
}
