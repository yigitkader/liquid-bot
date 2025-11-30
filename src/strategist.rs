use crate::balance_reservation::BalanceReservation;
use crate::config::Config;
use crate::domain::LiquidationOpportunity;
use crate::event::Event;
use crate::event_bus::EventBus;
use crate::protocol::Protocol;
use crate::solana_client::SolanaClient;
use crate::utils;
use crate::wallet::WalletBalanceChecker;
use anyhow::Result;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
use tokio::sync::broadcast;

async fn get_debt_reserve(
    obligation_address: &str,
    rpc_client: Arc<SolanaClient>,
) -> Option<Pubkey> {
    let obligation_pubkey = utils::parse_pubkey(obligation_address).ok()?;
    let obligation_account = rpc_client.get_account(&obligation_pubkey).await.ok()?;
    
    use crate::protocols::solend::solend_idl::SolendObligation;
    let obligation = SolendObligation::from_account_data(&obligation_account.data).ok()?;
    obligation.borrows.first().map(|b| b.borrow_reserve)
}

async fn check_oracle_price(
    reserve_pubkey: &Pubkey,
    config: &Config,
    rpc_client: Arc<SolanaClient>,
) -> Result<Option<u16>> {
    use crate::protocols::oracle_helper::{get_oracle_accounts_from_reserve, read_oracle_price};
    use crate::protocols::reserve_helper::parse_reserve_account;
    
    let reserve_account = rpc_client.get_account(reserve_pubkey).await?;
    let reserve_info = parse_reserve_account(reserve_pubkey, &reserve_account).await?;
    let (pyth, switchboard) = get_oracle_accounts_from_reserve(&reserve_info)?;
    
    // ✅ CRITICAL: If no oracle accounts found, return None immediately
    // This is different from oracle accounts existing but price read failing
    if pyth.is_none() && switchboard.is_none() {
        log::debug!(
            "No oracle accounts found for reserve {}. Cannot proceed without oracle data.",
            reserve_pubkey
        );
        return Ok(None); // Oracle accounts yok - bu durumda None döndür
    }
    
    // Oracle accounts exist, try to read price
    if let Some(price) = read_oracle_price(
        pyth.as_ref(),
        switchboard.as_ref(),
        rpc_client,
        Some(config),
    )
    .await?
    {
        let age_seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64 - price.timestamp;
        
        if age_seconds > config.max_oracle_age_seconds as i64 {
            return Err(anyhow::anyhow!(
                "oracle price too old: {}s (max: {}s)",
                age_seconds,
                config.max_oracle_age_seconds
            ));
        }
        
        let confidence_bps = ((price.confidence / price.price) * 10000.0) as u16;
        return Ok(Some(confidence_bps));
    }
    
    // Oracle accounts exist but price read failed (e.g., oracle data corrupted, network issue)
    // This is different from no oracle accounts - return error to indicate data issue
    Err(anyhow::anyhow!(
        "Oracle accounts exist for reserve {} but price read failed. This may indicate oracle data corruption or network issue.",
        reserve_pubkey
    ))
}

fn check_slippage(
    dex_slippage_bps: u16,
    oracle_confidence_bps: Option<u16>,
    config: &Config,
) -> Result<()> {
    let oracle_confidence = oracle_confidence_bps.unwrap_or(0);
    let total_slippage_bps = dex_slippage_bps + oracle_confidence;
    let final_slippage_bps = ((total_slippage_bps as f64 * config.slippage_final_multiplier) as u16)
        .min(u16::MAX);
    
    if final_slippage_bps > config.max_slippage_bps {
        return Err(anyhow::anyhow!(
            "total slippage {} bps > max {} bps (dex: {} bps, oracle: {} bps, multiplier: {:.2}x)",
            final_slippage_bps,
            config.max_slippage_bps,
            dex_slippage_bps,
            oracle_confidence,
            config.slippage_final_multiplier
        ));
    }
    
    Ok(())
}

async fn validate_token_accounts(
    debt_mint: &Pubkey,
    collateral_mint: &Pubkey,
    required_debt_amount: u64,
    wallet_balance_checker: &WalletBalanceChecker,
) -> Result<()> {
    if !wallet_balance_checker.ensure_token_account_exists(debt_mint).await? {
        return Err(anyhow::anyhow!("debt token account doesn't exist for mint {}", debt_mint));
    }
    
    let debt_balance = wallet_balance_checker.get_token_balance(debt_mint).await?;
    if debt_balance < required_debt_amount {
        return Err(anyhow::anyhow!(
            "insufficient debt token balance: required={}, available={}",
            required_debt_amount,
            debt_balance
        ));
    }
    
    if !wallet_balance_checker.ensure_token_account_exists(collateral_mint).await? {
        return Err(anyhow::anyhow!(
            "collateral token account doesn't exist for mint {}",
            collateral_mint
        ));
    }
    
    Ok(())
}

async fn reserve_balance(
    debt_mint: &Pubkey,
    required_amount: u64,
    min_sol_reserve: u64,
    wallet_balance_checker: &WalletBalanceChecker,
    balance_reservation: &BalanceReservation,
) -> Result<()> {
    let sol_balance = wallet_balance_checker.get_sol_balance().await?;
    if sol_balance < min_sol_reserve {
        return Err(anyhow::anyhow!(
            "insufficient SOL: required={}, available={}",
            min_sol_reserve,
            sol_balance
        ));
    }
    
    match balance_reservation
        .try_reserve_with_check(debt_mint, required_amount, wallet_balance_checker)
        .await?
    {
        Some(_guard) => Ok(()),
        None => {
            let reserved = balance_reservation.get_reserved(debt_mint).await;
            let current_balance = wallet_balance_checker.get_token_balance(debt_mint).await.unwrap_or(0);
            let available = balance_reservation.get_available(debt_mint, current_balance).await;
            Err(anyhow::anyhow!(
                "insufficient capital: required={}, available={}, reserved={}, actual={}",
                required_amount,
                available,
                reserved,
                current_balance
            ))
        }
    }
}

pub async fn run_strategist(
    mut receiver: broadcast::Receiver<Event>,
    bus: EventBus,
    config: Config,
    wallet_balance_checker: Arc<WalletBalanceChecker>,
    rpc_client: Arc<SolanaClient>,
    _protocol: Arc<dyn Protocol>,
    balance_reservation: Arc<BalanceReservation>,
) -> Result<()> {
    loop {
        match receiver.recv().await {
            Ok(Event::PotentiallyLiquidatable(opportunity)) => {
                let result = validate_opportunity(
                    &opportunity,
                    &config,
                    rpc_client.clone(),
                    &wallet_balance_checker,
                    &balance_reservation,
                )
                .await;
                
                match result {
                    Ok(()) => {
                        log::info!(
                            "Liquidation opportunity approved: profit=${:.2}, account={}",
                            opportunity.estimated_profit_usd,
                            opportunity.account_position.account_address
                        );
                        bus.publish(Event::ExecuteLiquidation(opportunity))?;
                    }
                    Err(e) => {
                        log::debug!(
                            "Liquidation opportunity rejected: account={}, reason={}",
                            opportunity.account_position.account_address,
                            e
                        );
                    }
                }
            }
            Ok(_) => {}
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

async fn validate_opportunity(
    opportunity: &LiquidationOpportunity,
    config: &Config,
    rpc_client: Arc<SolanaClient>,
    wallet_balance_checker: &WalletBalanceChecker,
    balance_reservation: &BalanceReservation,
) -> Result<()> {
    if opportunity.estimated_profit_usd < config.min_profit_usd {
        return Err(anyhow::anyhow!(
            "profit ${:.2} < min ${:.2}",
            opportunity.estimated_profit_usd,
            config.min_profit_usd
        ));
    }
    
    let estimated_dex_slippage_bps =
        (opportunity.liquidation_bonus * config.slippage_estimation_multiplier * 10000.0) as u16;
    
    let oracle_confidence_bps = if let Some(reserve_pubkey) =
        get_debt_reserve(&opportunity.account_position.account_address, rpc_client.clone()).await
    {
        match check_oracle_price(&reserve_pubkey, config, rpc_client.clone()).await {
            Ok(Some(confidence)) => Some(confidence),
            Ok(None) => {
                // ✅ No oracle accounts found for reserve - reject opportunity
                // This is the case where get_oracle_accounts_from_reserve returned (None, None)
                log::warn!(
                    "No oracle accounts found for reserve {}. Cannot proceed with liquidation without oracle data. Rejecting opportunity.",
                    reserve_pubkey
                );
                return Err(anyhow::anyhow!(
                    "No oracle accounts found for reserve {} - cannot proceed without price data",
                    reserve_pubkey
                ));
            }
            Err(e) => {
                // Oracle accounts exist but price read failed (e.g., oracle too old, data corrupted)
                log::error!("Failed to check oracle price for reserve {}: {}", reserve_pubkey, e);
                return Err(e);
            }
        }
    } else {
        // Could not find debt reserve - this is a different issue
        log::warn!(
            "Could not find debt reserve for obligation {}. Cannot proceed without reserve data. Rejecting opportunity.",
            opportunity.account_position.account_address
        );
        return Err(anyhow::anyhow!(
            "Could not find debt reserve for obligation {} - cannot proceed without reserve data",
            opportunity.account_position.account_address
        ));
    };
    
    // ✅ At this point, oracle_confidence_bps should always be Some(...)
    // If it's None, that's a logic error - we should have rejected earlier
    let oracle_confidence_bps = oracle_confidence_bps.expect("Oracle confidence should be Some at this point - this is a logic error");
    
    check_slippage(estimated_dex_slippage_bps, Some(oracle_confidence_bps), config)?;
    
    let debt_mint = utils::parse_pubkey(&opportunity.target_debt_mint)?;
    let collateral_mint = utils::parse_pubkey(&opportunity.target_collateral_mint)?;
    
    validate_token_accounts(
        &debt_mint,
        &collateral_mint,
        opportunity.max_liquidatable_amount,
        wallet_balance_checker,
    )
    .await?;
    
    reserve_balance(
        &debt_mint,
        opportunity.max_liquidatable_amount,
        config.min_reserve_lamports,
        wallet_balance_checker,
        balance_reservation,
    )
    .await?;
    
    Ok(())
}
