use anyhow::Result;
use tokio::sync::broadcast;
use std::sync::Arc;
use solana_sdk::pubkey::Pubkey;
use crate::config::Config;
use crate::event::Event;
use crate::event_bus::EventBus;
use crate::wallet::WalletBalanceChecker;
use crate::solana_client::SolanaClient;
use crate::protocol::Protocol;

/// Strategist worker - PotentiallyLiquidatable event'lerini iş kurallarına göre filtreler
pub async fn run_strategist(
    mut receiver: broadcast::Receiver<Event>,
    bus: EventBus,
    config: Config,
    wallet_balance_checker: Arc<WalletBalanceChecker>,
    rpc_client: Arc<SolanaClient>,
    protocol: Arc<dyn Protocol>,
) -> Result<()> {
    const MIN_RESERVE_LAMPORTS: u64 = 1_000_000; // 0.001 SOL minimum rezerv (transaction fee için)
    loop {
        match receiver.recv().await {
            Ok(Event::PotentiallyLiquidatable(opportunity)) => {
                // İş kuralları kontrolü
                let mut approved = true;
                let mut rejection_reason = String::new();
                
                // 1. Minimum profit kontrolü
                if opportunity.estimated_profit_usd < config.min_profit_usd {
                    approved = false;
                    rejection_reason = format!(
                        "profit ${:.2} < min ${:.2}",
                        opportunity.estimated_profit_usd,
                        config.min_profit_usd
                    );
                }
                
                if approved {
                    let estimated_slippage_bps = (opportunity.liquidation_bonus * 0.5 * 10000.0) as u16;
                    if let Ok(debt_mint) = Pubkey::try_from(opportunity.target_debt_mint.as_str()) {
                        use crate::protocols::oracle_helper::{get_oracle_accounts_from_mint, read_oracle_price};
                        if let Ok((pyth, switchboard)) = get_oracle_accounts_from_mint(&debt_mint) {
                            if let Ok(Some(price)) = read_oracle_price(pyth.as_ref(), switchboard.as_ref(), Arc::clone(&rpc_client)).await {
                                let confidence_slippage_bps = ((price.confidence / price.price) * 10000.0) as u16;
                                let age_seconds = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64 - price.timestamp;
                                
                                if age_seconds > 300 {
                                    approved = false;
                                    rejection_reason = format!("oracle price too old: {}s", age_seconds);
                                } else if confidence_slippage_bps > config.max_slippage_bps {
                                    approved = false;
                                    rejection_reason = format!("oracle slippage {} bps > max {} bps", confidence_slippage_bps, config.max_slippage_bps);
                                }
                            }
                        }
                    }
                    if approved && estimated_slippage_bps > config.max_slippage_bps {
                        approved = false;
                        rejection_reason = format!("estimated slippage {} bps > max {} bps", estimated_slippage_bps, config.max_slippage_bps);
                    }
                }
                
                // 3. Sermaye kontrolü (FR-4: Likidasyon için gerekli sermaye mevcut)
                if approved {
                    // Likidasyon için gerekli sermaye:
                    // 1. Debt token amount'u (max_liquidatable_amount) - debt ödemek için
                    // 2. SOL balance (transaction fee için)
                    
                    // Debt token mint'ini parse et
                    let debt_mint = match Pubkey::try_from(opportunity.target_debt_mint.as_str()) {
                        Ok(mint) => mint,
                        Err(e) => {
                            log::warn!("Invalid debt mint address: {}, rejecting opportunity", e);
                            approved = false;
                            rejection_reason = format!("invalid debt mint: {}", opportunity.target_debt_mint);
                            continue;
                        }
                    };
                    
                    // Gerekli debt token amount'u (native units)
                    let required_debt_amount = opportunity.max_liquidatable_amount;
                    
                    // Capital check: Debt token balance + SOL balance (transaction fee için)
                    match wallet_balance_checker.has_sufficient_capital_for_liquidation(
                        &debt_mint,
                        required_debt_amount,
                        MIN_RESERVE_LAMPORTS,
                    ).await {
                        Ok(true) => {
                            // Sermaye yeterli
                            log::debug!(
                                "Sufficient capital: debt_mint={}, required_debt={}, min_sol_reserve={}",
                                debt_mint,
                                required_debt_amount,
                                MIN_RESERVE_LAMPORTS
                            );
                        }
                        Ok(false) => {
                            approved = false;
                            let (available_debt, available_sol) = wallet_balance_checker
                                .get_available_capital_for_liquidation(&debt_mint, MIN_RESERVE_LAMPORTS)
                                .await
                                .unwrap_or((0, 0));
                            rejection_reason = format!(
                                "insufficient capital: required_debt={} (mint={}), available_debt={}, required_sol={}, available_sol={}",
                                required_debt_amount,
                                debt_mint,
                                available_debt,
                                MIN_RESERVE_LAMPORTS,
                                available_sol
                            );
                        }
                        Err(e) => {
                            log::warn!("Failed to check wallet balance: {}, rejecting opportunity", e);
                            approved = false;
                            rejection_reason = format!("balance check failed: {}", e);
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

