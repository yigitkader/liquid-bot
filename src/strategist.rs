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
                    let estimated_slippage_bps =
                        (opportunity.liquidation_bonus * config.slippage_estimation_multiplier * 10000.0) as u16;
                    let debt_mint_str = &opportunity.target_debt_mint;
                    let mut debt_reserve_pubkey: Option<Pubkey> = None;

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
                                                    // Pyth confidence = %68 confidence interval (1 sigma)
                                                    // %95 confidence interval için Z-score kullanılmalı (default: 1.96)
                                                    let confidence_95 = price.confidence * config.z_score_95;
                                                    let confidence_slippage_bps = ((confidence_95
                                                        / price.price)
                                                        * 10000.0)
                                                        as u16;
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
                                                    } else if confidence_slippage_bps
                                                        > config.max_slippage_bps
                                                    {
                                                        approved = false;
                                                        rejection_reason = format!(
                                                            "oracle slippage {} bps > max {} bps",
                                                            confidence_slippage_bps,
                                                            config.max_slippage_bps
                                                        );
                                                    }
                                                } else {
                                                    log::warn!("Failed to read oracle price from reserve {}", reserve_pubkey);
                                                }
                                            }
                                            Err(e) => {
                                                log::warn!("Failed to get oracle accounts from reserve {}: {}", reserve_pubkey, e);
                                                // Fallback: Hardcoded mapping dene (sadece 5 token için çalışır)
                                                if let Ok(debt_mint) =
                                                    Pubkey::try_from(debt_mint_str.as_str())
                                                {
                                                    use crate::protocols::oracle_helper::get_oracle_accounts_from_mint;
                                                    if let Ok((pyth, switchboard)) =
                                                        get_oracle_accounts_from_mint(&debt_mint, Some(&config))
                                                    {
                                                        if let Ok(Some(price)) = read_oracle_price(
                                                            pyth.as_ref(),
                                                            switchboard.as_ref(),
                                                            Arc::clone(&rpc_client),
                                                            Some(&config),
                                                        )
                                                        .await
                                                        {
                                                            // Pyth confidence = %68 confidence interval (1 sigma)
                                                            // %95 confidence interval için Z-score kullanılmalı (default: 1.96)
                                                            let confidence_95 = price.confidence * config.z_score_95;
                                                            let confidence_slippage_bps =
                                                                ((confidence_95 / price.price)
                                                                    * 10000.0)
                                                                    as u16;
                                                            let age_seconds =
                                                                std::time::SystemTime::now()
                                                                    .duration_since(
                                                                        std::time::UNIX_EPOCH,
                                                                    )
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
                                                            } else if confidence_slippage_bps
                                                                > config.max_slippage_bps
                                                            {
                                                                approved = false;
                                                                rejection_reason = format!("oracle slippage {} bps > max {} bps", confidence_slippage_bps, config.max_slippage_bps);
                                                            }
                                                        }
                                                    } else {
                                                        log::warn!("⚠️  No oracle accounts found for debt mint {} (not in hardcoded mapping). Reserve-based lookup failed. This token may not be supported.", debt_mint);
                                                    }
                                                }
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

                    if approved && estimated_slippage_bps > config.max_slippage_bps {
                        approved = false;
                        rejection_reason = format!(
                            "estimated slippage {} bps > max {} bps",
                            estimated_slippage_bps, config.max_slippage_bps
                        );
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
                        // Use balance reservation to prevent race conditions
                        // This ensures that if multiple opportunities require the same token,
                        // only one can be approved at a time
                        let debt_balance = match wallet_balance_checker.get_token_balance(&debt_mint).await {
                            Ok(balance) => balance,
                            Err(e) => {
                                log::warn!("Failed to get debt token balance: {}, rejecting opportunity", e);
                                approved = false;
                                rejection_reason = format!("balance check failed: {}", e);
                                continue;
                            }
                        };

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

                        // Try to reserve the required debt amount
                        // This prevents race conditions when multiple opportunities are processed in parallel
                        if balance_reservation.reserve(&debt_mint, required_debt_amount, debt_balance).await {
                            log::debug!(
                                "Balance reserved: debt_mint={}, amount={}, available={}",
                                debt_mint,
                                required_debt_amount,
                                debt_balance
                            );
                        } else {
                            approved = false;
                            let reserved = balance_reservation.get_reserved(&debt_mint).await;
                            let available = balance_reservation.get_available(&debt_mint, debt_balance).await;
                            rejection_reason = format!(
                                "insufficient capital (race condition prevented): required_debt={} (mint={}), available={}, reserved={}, actual={}",
                                required_debt_amount,
                                debt_mint,
                                available,
                                reserved,
                                debt_balance
                            );
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
