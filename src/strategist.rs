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
    _protocol: Arc<dyn Protocol>,
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
                    
                    // ✅ CRITICAL FIX: Oracle'ları reserve account'tan oku (hardcoded mapping yerine)
                    // Hardcoded mapping sadece 5 token için çalışır (USDC, USDT, SOL, ETH, BTC)
                    // Diğer token'lar (BONK, RAY, SRM, etc.) için reserve'den okuma gerekli!
                    //
                    // Strategy:
                    // 1. target_debt_mint'i reserve pubkey olarak dene (bazı durumlarda bu reserve pubkey olabilir)
                    // 2. Başarısız olursa, obligation'dan reserve pubkey'ini al
                    // 3. Reserve account'unu parse et
                    // 4. Reserve'den oracle'ları al (tüm token'lar için çalışır!)
                    
                    let debt_mint_str = &opportunity.target_debt_mint;
                    let mut debt_reserve_pubkey: Option<Pubkey> = None;
                    
                    // Strategy 1: target_debt_mint'i reserve pubkey olarak dene
                    if let Ok(pubkey) = Pubkey::try_from(debt_mint_str.as_str()) {
                        // Bu pubkey bir reserve account olabilir, kontrol et
                        match rpc_client.get_account(&pubkey).await {
                            Ok(account) => {
                                // Account'un Solend program'ına ait olup olmadığını kontrol et
                                // (Basit kontrol: account size 619 bytes ise muhtemelen reserve)
                                if account.data.len() == 619 {
                                    debt_reserve_pubkey = Some(pubkey);
                                }
                            }
                            Err(_) => {}
                        }
                    }
                    
                    // Strategy 2: Obligation'dan reserve pubkey'ini al
                    if debt_reserve_pubkey.is_none() {
                        if let Ok(obligation_pubkey) = Pubkey::try_from(opportunity.account_position.account_address.as_str()) {
                            use crate::protocols::solend::solend_idl::SolendObligation;
                            
                            if let Ok(obligation_account) = rpc_client.get_account(&obligation_pubkey).await {
                                if let Ok(obligation) = SolendObligation::from_account_data(&obligation_account.data) {
                                    // İlk borrow reserve'ini al (genellikle doğru olan budur)
                                    if let Some(borrow) = obligation.borrows.first() {
                                        debt_reserve_pubkey = Some(borrow.borrow_reserve);
                                    }
                                }
                            }
                        }
                    }
                    
                    // Reserve'den oracle'ları al ve fiyat kontrolü yap
                    if let Some(reserve_pubkey) = debt_reserve_pubkey {
                        use crate::protocols::reserve_helper::parse_reserve_account;
                        use crate::protocols::oracle_helper::{get_oracle_accounts_from_reserve, read_oracle_price};
                        
                        match rpc_client.get_account(&reserve_pubkey).await {
                            Ok(reserve_account) => {
                                match parse_reserve_account(&reserve_pubkey, &reserve_account).await {
                                    Ok(reserve_info) => {
                                        // ✅ Reserve'den oracle'ları al (tüm token'lar için çalışır!)
                                        match get_oracle_accounts_from_reserve(&reserve_info) {
                                            Ok((pyth, switchboard)) => {
                                                if let Ok(Some(price)) = read_oracle_price(
                                                    pyth.as_ref(),
                                                    switchboard.as_ref(),
                                                    Arc::clone(&rpc_client)
                                                ).await {
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
                                                } else {
                                                    log::warn!("Failed to read oracle price from reserve {}", reserve_pubkey);
                                                }
                                            }
                                            Err(e) => {
                                                log::warn!("Failed to get oracle accounts from reserve {}: {}", reserve_pubkey, e);
                                                // Fallback: Hardcoded mapping dene (sadece 5 token için çalışır)
                                                if let Ok(debt_mint) = Pubkey::try_from(debt_mint_str.as_str()) {
                                                    use crate::protocols::oracle_helper::get_oracle_accounts_from_mint;
                                                    if let Ok((pyth, switchboard)) = get_oracle_accounts_from_mint(&debt_mint) {
                                                        if let Ok(Some(price)) = read_oracle_price(
                                                            pyth.as_ref(),
                                                            switchboard.as_ref(),
                                                            Arc::clone(&rpc_client)
                                                        ).await {
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
                                                    } else {
                                                        log::warn!("⚠️  No oracle accounts found for debt mint {} (not in hardcoded mapping). Reserve-based lookup failed. This token may not be supported.", debt_mint);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        log::warn!("Failed to parse reserve account {}: {}", reserve_pubkey, e);
                                    }
                                }
                            }
                            Err(e) => {
                                log::warn!("Failed to read reserve account {}: {}", reserve_pubkey, e);
                            }
                        }
                    } else {
                        log::warn!("Could not find debt reserve pubkey for debt mint {}. Skipping oracle check.", debt_mint_str);
                    }
                    
                    if approved && estimated_slippage_bps > config.max_slippage_bps {
                        approved = false;
                        rejection_reason = format!("estimated slippage {} bps > max {} bps", estimated_slippage_bps, config.max_slippage_bps);
                    }
                }
                
                // 3. Sermaye kontrolü (FR-4: Likidasyon için gerekli sermaye mevcut)
                if approved {
                    // Likidasyon için gerekli 3 şey:
                    // 1. Debt token amount'u (max_liquidatable_amount) - debt ödemek için
                    // 2. SOL balance (transaction fee için)
                    // 3. Collateral token account (seizable collateral almak için) - CRITICAL!
                    
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
                    
                    // Collateral token mint'ini parse et
                    let collateral_mint = match Pubkey::try_from(opportunity.target_collateral_mint.as_str()) {
                        Ok(mint) => mint,
                        Err(e) => {
                            log::warn!("Invalid collateral mint address: {}, rejecting opportunity", e);
                            approved = false;
                            rejection_reason = format!("invalid collateral mint: {}", opportunity.target_collateral_mint);
                            continue;
                        }
                    };
                    
                    // CRITICAL: Collateral token account var mı kontrol et
                    // Eğer yoksa, liquidation transaction başarısız olur çünkü
                    // instruction destination collateral account bekler.
                    match wallet_balance_checker.ensure_token_account_exists(&collateral_mint).await {
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
                            rejection_reason = format!("collateral token account check failed: {}", e);
                        }
                    }
                    
                    // Gerekli debt token amount'u (native units)
                    let required_debt_amount = opportunity.max_liquidatable_amount;
                    
                    // Capital check: Debt token balance + SOL balance (transaction fee için)
                    // NOTE: Bu kontrol sadece approved ise yapılır (collateral account check başarılıysa)
                    if approved {
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

