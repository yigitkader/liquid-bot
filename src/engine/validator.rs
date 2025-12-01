use crate::core::events::{Event, EventBus};
use crate::core::types::Opportunity;
use crate::core::config::Config;
use crate::blockchain::rpc_client::RpcClient;
use crate::strategy::balance_manager::BalanceManager;
use crate::protocol::solend::accounts::get_associated_token_address;
use anyhow::{Result, Context};
use std::sync::Arc;
use tokio::sync::broadcast;
use solana_sdk::pubkey::Pubkey;

pub struct Validator {
    event_bus: EventBus,
    balance_manager: Arc<BalanceManager>,
    config: Config,
    rpc: Arc<RpcClient>,
}

impl Validator {
    pub fn new(
        event_bus: EventBus,
        balance_manager: Arc<BalanceManager>,
        config: Config,
        rpc: Arc<RpcClient>,
    ) -> Self {
        Validator {
            event_bus,
            balance_manager,
            config,
            rpc,
        }
    }

    pub async fn run(&self) -> Result<()> {
        let mut receiver = self.event_bus.subscribe();

        loop {
            match receiver.recv().await {
                Ok(Event::OpportunityFound { opportunity }) => {
                    log::info!(
                        "Validator: received OpportunityFound for position {} (debt_mint={}, collateral_mint={}, max_liquidatable={}, est_profit={:.4})",
                        opportunity.position.address,
                        opportunity.debt_mint,
                        opportunity.collateral_mint,
                        opportunity.max_liquidatable,
                        opportunity.estimated_profit
                    );

                    match self.validate(&opportunity).await {
                        Ok(()) => {
                            log::info!(
                                "Validator: opportunity approved for position {}",
                                opportunity.position.address
                            );
                            self.event_bus.publish(Event::OpportunityApproved {
                                opportunity,
                            })?;
                        }
                        Err(e) => {
                            log::info!(
                                "Validator: opportunity rejected for position {}: {}",
                                opportunity.position.address,
                                e
                            );
                        }
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    log::warn!("Validator lagged, skipped {} events", skipped);
                }
                Err(broadcast::error::RecvError::Closed) => {
                    log::error!("Event bus closed, validator shutting down");
                    break;
                }
            }
        }

        Ok(())
    }

    async fn validate(&self, opp: &Opportunity) -> Result<()> {
        log::info!("🔍 VALIDATION DEBUG START for position: {}", opp.position.address);
        log::info!("   Debt Mint: {}", opp.debt_mint);
        log::info!("   Collateral Mint: {}", opp.collateral_mint);
        log::info!("   Max Liquidatable: {}", opp.max_liquidatable);

        // Step 1: Balance check
        log::info!("   Step 1/7: Checking balance...");
        match self.has_sufficient_balance(&opp.debt_mint, opp.max_liquidatable).await {
            Ok(_) => log::info!("   ✅ Balance OK"),
            Err(e) => {
                log::error!("   ❌ Balance FAILED: {}", e);
                return Err(e);
            }
        }
        
        // Step 2: Oracle freshness - debt
        log::info!("   Step 2/7: Checking debt oracle freshness...");
        match self.check_oracle_freshness(&opp.debt_mint).await {
            Ok(_) => log::info!("   ✅ Debt oracle OK"),
            Err(e) => {
                log::error!("   ❌ Debt oracle FAILED: {}", e);
                log::error!("   Trying to fetch oracle account info...");
                
                // Debug: Oracle account var mı kontrol et
                use crate::protocol::oracle::get_pyth_oracle_account;
                if let Ok(Some(oracle_account)) = get_pyth_oracle_account(&opp.debt_mint, Some(&self.config)) {
                    log::error!("   Oracle account found: {}", oracle_account);
                    match self.rpc.get_account(&oracle_account).await {
                        Ok(acc) => log::error!("   Oracle account EXISTS: {} bytes, owner: {}", 
                            acc.data.len(), acc.owner),
                        Err(fetch_err) => log::error!("   ⚠️  ORACLE ACCOUNT FETCH FAILED: {} (THIS IS THE PROBLEM!)", fetch_err),
                    }
                } else {
                    log::error!("   No oracle account configured for mint: {}", opp.debt_mint);
                }
                return Err(e);
            }
        }
        
        // Step 3: Oracle freshness - collateral
        log::info!("   Step 3/7: Checking collateral oracle freshness...");
        match self.check_oracle_freshness(&opp.collateral_mint).await {
            Ok(_) => log::info!("   ✅ Collateral oracle OK"),
            Err(e) => {
                log::error!("   ❌ Collateral oracle FAILED: {}", e);
                return Err(e);
            }
        }
        
        // Step 4: ATA verification - debt (NON-BLOCKING - sadece warning)
        log::info!("   Step 4/7: Verifying debt ATA...");
        if let Err(e) = self.verify_ata_exists(&opp.debt_mint).await {
            log::warn!("   ⚠️  Debt ATA verification failed (non-critical): {}", e);
            // Devam et - ATA eksikse Solana runtime handle eder
        } else {
            log::info!("   ✅ Debt ATA OK");
        }
        
        // Step 5: ATA verification - collateral (NON-BLOCKING - sadece warning)
        log::info!("   Step 5/7: Verifying collateral ATA...");
        if let Err(e) = self.verify_ata_exists(&opp.collateral_mint).await {
            log::warn!("   ⚠️  Collateral ATA verification failed (non-critical): {}", e);
            // Devam et - ATA eksikse Solana runtime handle eder
        } else {
            log::info!("   ✅ Collateral ATA OK");
        }
        
        // Step 6: Slippage check
        log::info!("   Step 6/7: Checking slippage...");
        let slippage = match self.get_realtime_slippage(opp).await {
            Ok(s) => s,
            Err(e) => {
                log::error!("   ❌ Slippage calculation FAILED: {}", e);
                return Err(e);
            }
        };
        if slippage > self.config.max_slippage_bps {
            log::error!("   ❌ Slippage too high: {} bps (max: {})", slippage, self.config.max_slippage_bps);
            return Err(anyhow::anyhow!("Slippage too high: {} bps", slippage));
        }
        log::info!("   ✅ Slippage OK: {} bps", slippage);
        
        // Step 7: Reserve balance
        log::info!("   Step 7/7: Reserving balance...");
        match self.balance_manager.reserve(&opp.debt_mint, opp.max_liquidatable).await {
            Ok(_) => log::info!("   ✅ Balance reserved"),
            Err(e) => {
                log::error!("   ❌ Balance reservation FAILED: {}", e);
                return Err(e);
            }
        }
        
        log::info!("🎉 VALIDATION PASSED!");
        Ok(())
    }

    async fn has_sufficient_balance(&self, mint: &Pubkey, amount: u64) -> Result<()> {
        let available = self.balance_manager.get_available_balance(mint).await?;
        if available < amount {
            return Err(anyhow::anyhow!(
                "Insufficient balance for mint {}: need {}, have {}",
                mint,
                amount,
                available
            ));
        }
        log::debug!(
            "Validator: sufficient balance for mint {}: need {}, available {}",
            mint,
            amount,
            available
        );
        Ok(())
    }

    async fn verify_ata_exists(&self, mint: &Pubkey) -> Result<()> {
        let wallet_pubkey_str = self.config.test_wallet_pubkey
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or("11111111111111111111111111111111");
        let wallet_pubkey = wallet_pubkey_str.parse::<Pubkey>()
            .map_err(|_| anyhow::anyhow!("Invalid wallet pubkey"))?;
        
        // FIX: Config parametresini doğru geçiyoruz
        let ata = get_associated_token_address(&wallet_pubkey, mint, Some(&self.config))
            .context("Failed to derive ATA address")?;
        
        log::debug!(
            "Validator: checking ATA existence: owner_wallet={}, mint={}, ata={}",
            wallet_pubkey,
            mint,
            ata
        );
        
        match self.rpc.get_account(&ata).await {
            Ok(account) => {
                if account.data.is_empty() {
                    log::warn!("⚠️  ATA exists but is empty: {}", ata);
                    // Empty ATA is OK - Solana runtime will handle it
                    return Ok(());
                }
                log::debug!(
                    "Validator: ATA exists and non-empty: owner_wallet={}, mint={}, ata={}, data_len={}",
                    wallet_pubkey,
                    mint,
                    ata,
                    account.data.len()
                );
                Ok(())
            }
            Err(e) => {
                // ATA bulunamadı - bu NORMAL olabilir!
                // Solana transaction'da otomatik create edilir (eğer gerekirse)
                log::warn!(
                    "⚠️  ATA not found for mint {} ({}), will be auto-created if needed", 
                    mint, ata
                );
                log::warn!("   Error: {}", e);
                
                // ✅ YENİ: Warning ver ama devam et
                // Solana'da ATA yoksa transaction içinde create edilir
                Ok(())
            }
        }
    }

    async fn check_oracle_freshness(&self, mint: &Pubkey) -> Result<()> {
        use crate::protocol::oracle::{get_pyth_oracle_account, read_pyth_price};
        
        let oracle_account = match get_pyth_oracle_account(mint, Some(&self.config)) {
            Ok(Some(account)) => account,
            Ok(None) => {
                log::warn!("No Pyth oracle found for mint: {}, skipping freshness check", mint);
                return Ok(());
            }
            Err(e) => {
                log::error!("Failed to get Pyth oracle account for mint {}: {}", mint, e);
                return Err(anyhow::anyhow!("Failed to get Pyth oracle account for mint {}: {}", mint, e));
            }
        };
        
        log::debug!(
            "Validator: checking oracle freshness for mint {} (oracle={})",
            mint,
            oracle_account
        );
        
        // Debug: Oracle account'u fetch etmeyi dene
        match self.rpc.get_account(&oracle_account).await {
            Ok(acc) => {
                log::debug!(
                    "Validator: oracle account exists: {} bytes, owner: {}",
                    acc.data.len(),
                    acc.owner
                );
            }
            Err(e) => {
                log::error!(
                    "Validator: failed to fetch oracle account {} for mint {}: {}",
                    oracle_account,
                    mint,
                    e
                );
                return Err(anyhow::anyhow!(
                    "Failed to fetch oracle account {} for mint {}: {}",
                    oracle_account,
                    mint,
                    e
                ));
            }
        }
        
        let max_age = self.config.max_oracle_age_seconds;
        let price_data = match read_pyth_price(&oracle_account, Arc::clone(&self.rpc), Some(&self.config)).await {
            Ok(Some(data)) => data,
            Ok(None) => {
                return Err(anyhow::anyhow!(
                    "Failed to read oracle price data for mint {} (oracle={}) - price data is None",
                    mint,
                    oracle_account
                ));
            }
            Err(e) => {
                log::error!(
                    "Validator: failed to read Pyth price for mint {} (oracle={}): {}",
                    mint,
                    oracle_account,
                    e
                );
                return Err(anyhow::anyhow!(
                    "Failed to read oracle price data for mint {} (oracle={}): {}",
                    mint,
                    oracle_account,
                    e
                ));
            }
        };
        
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| anyhow::anyhow!("Failed to get current time: {}", e))?
            .as_secs() as i64;
        
        let age_seconds = current_time - price_data.timestamp;
        if age_seconds > max_age as i64 {
            log::error!(
                "Validator: oracle price data is stale for mint {} (oracle {}): {} seconds old (max: {} seconds)",
                mint,
                oracle_account,
                age_seconds,
                max_age
            );
            return Err(anyhow::anyhow!(
                "Oracle price data is stale for mint {} (oracle {}): {} seconds old (max: {} seconds)",
                mint,
                oracle_account,
                age_seconds,
                max_age
            ));
        }
        
        log::debug!(
            "Validator: oracle price is fresh for mint {} (oracle={}, age={}s, max_age={}s, price={:.4}, confidence={:.4})",
            mint,
            oracle_account,
            age_seconds,
            max_age,
            price_data.price,
            price_data.confidence
        );
        
        Ok(())
    }

    async fn get_realtime_slippage(&self, opp: &Opportunity) -> Result<u16> {
        use crate::strategy::slippage_estimator::SlippageEstimator;
        
        let estimator = SlippageEstimator::new(self.config.clone());
        
        // Get base DEX slippage estimate
        let base_slippage = estimator.estimate_dex_slippage(
            opp.debt_mint,
            opp.collateral_mint,
            opp.seizable_collateral,
        ).await
            .context("Failed to estimate real-time slippage")?;

        // Read oracle confidence for debt mint and add to slippage
        let oracle_confidence = estimator.read_oracle_confidence(opp.debt_mint).await
            .context("Failed to read oracle confidence")?;

        // Add oracle confidence to base slippage
        // Oracle confidence represents price uncertainty, which should be added to slippage
        let total_slippage = base_slippage.saturating_add(oracle_confidence);
        
        // Cap at max_slippage_bps
        let final_slippage = total_slippage.min(self.config.max_slippage_bps);

        log::debug!(
            "Validator: slippage breakdown for position {}: base={} bps, oracle_confidence={} bps, total={} bps (max {} bps)",
            opp.position.address,
            base_slippage,
            oracle_confidence,
            final_slippage,
            self.config.max_slippage_bps
        );
        
        Ok(final_slippage)
    }
}
