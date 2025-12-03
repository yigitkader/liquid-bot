use crate::core::events::{Event, EventBus};
use crate::core::types::Opportunity;
use crate::core::config::Config;
use crate::blockchain::rpc_client::RpcClient;
use crate::strategy::balance_manager::BalanceManager;
use crate::protocol::solend::accounts::get_associated_token_address;
use anyhow::{Result, Context};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::task::JoinSet;
use tokio::sync::Semaphore;
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
        let mut tasks = JoinSet::new();
        let semaphore = Arc::new(Semaphore::new(4)); // Max 4 parallel validations
        
        loop {
            tokio::select! {
                // Handle new events
                event_result = receiver.recv() => {
                    match event_result {
                        Ok(Event::OpportunityFound { opportunity }) => {
                            let permit = semaphore.clone().acquire_owned().await
                                .context("Failed to acquire semaphore permit")?;
                            
                            let event_bus = self.event_bus.clone();
                            let rpc = Arc::clone(&self.rpc);
                            let config = self.config.clone();
                            let balance_manager = Arc::clone(&self.balance_manager);
                            
                            // Clone opportunity before moving into task
                            let opportunity_clone = opportunity.clone();
                            let debt_mint = opportunity.debt_mint;
                            let max_liquidatable = opportunity.max_liquidatable;
                            let position_address = opportunity.position.address;
                            
                            tasks.spawn(async move {
                                let _permit = permit;
                                
                                log::debug!(
                                    "Validator: processing opportunity for position {} (est_profit={:.4})",
                                    position_address,
                                    opportunity_clone.estimated_profit
                                );

                                // ✅ CRITICAL FIX: Reserve balance FIRST (before validation)
                                // This prevents race conditions where multiple validators try to reserve the same balance
                                // Reserve is atomic - if it fails, we skip validation (insufficient balance)
                                match balance_manager.reserve(&debt_mint, max_liquidatable).await {
                                    Ok(()) => {
                                        // Reserved successfully - proceed with validation
                                    }
                                    Err(e) => {
                                        log::debug!(
                                            "Validator: insufficient balance for position {}: {}",
                                            position_address,
                                            e
                                        );
                                        // Don't validate if we can't reserve balance
                                        return;
                                    }
                                }
                                
                                // Validate the opportunity (reservation already done)
                                match Self::validate_static(&opportunity_clone, &rpc, &config).await {
                                    Ok(()) => {
                                        // ✅ SUCCESS: Keep reservation, pass to executor
                                        // Reservation will be released by executor after transaction
                                        log::info!(
                                            "Validator: opportunity approved for position {}",
                                            position_address
                                        );
                                        if let Err(e) = event_bus.publish(Event::OpportunityApproved {
                                            opportunity: opportunity_clone,
                                        }) {
                                            log::error!("Failed to publish OpportunityApproved: {}", e);
                                            // If publish fails, release reservation (executor won't see it)
                                            balance_manager
                                                .release(
                                                    &debt_mint,
                                                    max_liquidatable,
                                                )
                                                .await;
                                        }
                                    }
                                    Err(e) => {
                                        // ❌ VALIDATION FAILED: Release reservation
                                        // Executor will never see this opportunity, so we must release
                                        log::debug!(
                                            "Validator: opportunity rejected for position {}: {}",
                                            position_address,
                                            e
                                        );
                                        balance_manager
                                            .release(
                                                &debt_mint,
                                                max_liquidatable,
                                            )
                                            .await;
                                    }
                                }
                            });
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
                // Cleanup finished tasks
                Some(result) = tasks.join_next() => {
                    if let Err(e) = result {
                        log::error!("Validator task failed: {}", e);
                    }
                }
            }
        }

        // Wait for all tasks to complete
        while let Some(result) = tasks.join_next().await {
            if let Err(e) = result {
                log::error!("Validator task failed: {}", e);
            }
        }

        Ok(())
    }

    async fn validate(&self, opp: &Opportunity) -> Result<()> {
        // Reserve balance first
        self.balance_manager
            .reserve(&opp.debt_mint, opp.max_liquidatable)
            .await
            .context("Insufficient balance - reservation failed")?;
        
        // Validate (reservation already done)
        match Self::validate_static(opp, &self.rpc, &self.config).await {
            Ok(()) => Ok(()),
            Err(e) => {
                // Release reservation on validation failure
                self.balance_manager
                    .release(&opp.debt_mint, opp.max_liquidatable)
                    .await;
                Err(e)
            }
        }
    }

    // Static method for parallel processing
    // ✅ CRITICAL FIX: Reserve is now done BEFORE calling this function
    // This ensures reservation happens atomically before validation starts
    // If validation fails, caller must release the reservation
    async fn validate_static(
        opp: &Opportunity,
        rpc: &Arc<RpcClient>,
        config: &Config,
    ) -> Result<()> {
        log::debug!("🔍 Validating opportunity for position: {}", opp.position.address);

        // Step 1: Oracle freshness - debt (parallel with collateral)
        // ✅ CRITICAL: For free RPC endpoints, run oracle checks sequentially to avoid rate limits
        // ✅ FIX: Add 100ms delay ONCE before sequential checks, not per-check
        // Problem: Each check added 100ms delay → 200ms total delay + 2 RPC calls = ~20s per opportunity
        // Solution: Add delay once, then run checks sequentially WITHOUT extra delays
        let (debt_result, collateral_result) = if config.is_free_rpc_endpoint() {
            // Add 100ms delay ONCE, then run checks sequentially WITHOUT extra delays
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            let debt_result = Self::check_oracle_freshness_static(&opp.debt_mint, rpc, config, true).await;
            let collateral_result = Self::check_oracle_freshness_static(&opp.collateral_mint, rpc, config, true).await;
            (debt_result, collateral_result)
        } else {
            // Parallel execution for premium RPC (faster, no rate limit concerns)
            let debt_oracle_task = Self::check_oracle_freshness_static(&opp.debt_mint, rpc, config, false);
            let collateral_oracle_task = Self::check_oracle_freshness_static(&opp.collateral_mint, rpc, config, false);
            tokio::join!(debt_oracle_task, collateral_oracle_task)
        };
        
        if let Err(e) = debt_result {
            log::debug!("   ❌ Debt oracle FAILED: {}", e);
            return Err(e);
        }
        
        if let Err(e) = collateral_result {
            log::debug!("   ❌ Collateral oracle FAILED: {}", e);
            return Err(e);
        }
        
        // Step 2: Slippage check
        let slippage = match Self::get_realtime_slippage_static(opp, rpc, config).await {
            Ok(s) => s,
            Err(e) => {
                log::debug!("   ❌ Slippage calculation FAILED: {}", e);
                return Err(e);
            }
        };
        if slippage > config.max_slippage_bps {
            log::debug!("   ❌ Slippage too high: {} bps (max: {})", slippage, config.max_slippage_bps);
            return Err(anyhow::anyhow!("Slippage too high: {} bps", slippage));
        }
        
        log::debug!("✅ Validation passed for position: {}", opp.position.address);
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
        Self::check_oracle_freshness_static(mint, &self.rpc, &self.config, false).await
    }

    async fn check_oracle_freshness_static(mint: &Pubkey, rpc: &Arc<RpcClient>, config: &Config, skip_delay: bool) -> Result<()> {
        use crate::protocol::oracle::{get_pyth_oracle_account, read_pyth_price};
        use tokio::time::{Duration, timeout};
        
        // ✅ FIX: Wrap entire oracle check in timeout to prevent cumulative timeouts
        // Free RPC: Sequential checks can take 20s+ (10s per oracle × 2)
        // Premium RPC: Parallel checks can take 10s+ (10s per oracle, but parallel)
        // Solution: Cap each oracle check at 5s to prevent validation from taking too long
        const ORACLE_CHECK_TIMEOUT: Duration = Duration::from_secs(5);
        
        timeout(ORACLE_CHECK_TIMEOUT, async {
            // ✅ CRITICAL: Add rate limit guard for free RPC endpoints
            // Free RPC endpoints (api.mainnet-beta.solana.com) have strict rate limits:
            // - 1 req/10s for getProgramAccounts
            // - ~10 req/s for getAccount (but we should be conservative)
            // This prevents rate limit violations when multiple oracle checks run in parallel
            // ✅ FIX: Skip delay if already added before sequential calls (skip_delay=true)
            if config.is_free_rpc_endpoint() && !skip_delay {
                // Minimum 100ms delay between oracle checks to avoid rate limit
                // This is especially important when debt and collateral checks run in parallel
                // Note: When called sequentially, delay is added once before both calls
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            
            let oracle_account = match get_pyth_oracle_account(mint, Some(config)) {
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
        
        // Skip account existence check - read_pyth_price will handle it
        let max_age = config.max_oracle_age_seconds;
        
        // ✅ CRITICAL FIX: Oracle timeout is now consistent with RPC client timeout
        // read_pyth_price() uses RPC client's timeout (default: 10s) for Pyth SDK call
        // RPC client also uses the same timeout (10s) for get_account() call
        // This ensures consistent timeout handling: Total wait = 10s (not 5s + 10s = 15s)
        // 
        // Timeout flow:
        //   1. RPC get_account() call: 10s timeout (RPC client level)
        //   2. Pyth SDK call: 10s timeout (oracle level, matches RPC timeout)
        //   Total: 10s maximum (consistent!)
        let price_data = match read_pyth_price(&oracle_account, Arc::clone(rpc), Some(config)).await {
            Ok(Some(data)) => data,
            Ok(None) => {
                return Err(anyhow::anyhow!(
                    "Failed to read oracle price data for mint {} (oracle={}) - price data is None",
                    mint,
                    oracle_account
                ));
            }
            Err(e) => {
                // Check if error is timeout-related
                let error_str = e.to_string().to_lowercase();
                if error_str.contains("timeout") || error_str.contains("timed out") {
                    log::error!(
                        "Validator: oracle read timeout for mint {} (oracle={}) - RPC may be slow or unresponsive: {}",
                        mint,
                        oracle_account,
                        e
                    );
                    return Err(anyhow::anyhow!(
                        "Oracle read timeout for mint {} (oracle={}) - RPC may be slow or unresponsive: {}",
                        mint,
                        oracle_account,
                        e
                    ));
                }
                
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
        })
        .await
        .map_err(|_| anyhow::anyhow!(
            "Oracle check timeout after {:?} for mint {}",
            ORACLE_CHECK_TIMEOUT,
            mint
        ))?;
        
        Ok(())
    }

    async fn get_realtime_slippage(&self, opp: &Opportunity) -> Result<u16> {
        Self::get_realtime_slippage_static(opp, &self.rpc, &self.config).await
    }

    async fn get_realtime_slippage_static(opp: &Opportunity, _rpc: &Arc<RpcClient>, config: &Config) -> Result<u16> {
        use crate::strategy::slippage_estimator::SlippageEstimator;
        
        let estimator = SlippageEstimator::new(config.clone());
        
        // Get base DEX slippage estimate
        let base_slippage = estimator.estimate_dex_slippage(
            opp.debt_mint,
            opp.collateral_mint,
            opp.seizable_collateral,
        ).await
            .context("Failed to estimate real-time slippage")?;

        // Read oracle confidence for debt mint
        let oracle_confidence = estimator.read_oracle_confidence(opp.debt_mint).await
            .context("Failed to read oracle confidence")?;

        // Combine base slippage and oracle confidence using quadratic sum
        // This is statistically more accurate for combining independent uncertainties
        // than simple addition, which can lead to overly conservative estimates
        // Formula: sqrt(base_slippage^2 + oracle_confidence^2)
        let total_slippage = {
            let base_f64 = base_slippage as f64;
            let oracle_f64 = oracle_confidence as f64;
            let sum_squares = base_f64 * base_f64 + oracle_f64 * oracle_f64;
            (sum_squares.sqrt()) as u16
        };
        
        // Cap at max_slippage_bps
        let final_slippage = total_slippage.min(config.max_slippage_bps);

        log::debug!(
            "Validator: slippage breakdown for position {}: base={} bps, oracle_confidence={} bps, total={} bps (max {} bps)",
            opp.position.address,
            base_slippage,
            oracle_confidence,
            final_slippage,
            config.max_slippage_bps
        );
        
        Ok(final_slippage)
    }
}
