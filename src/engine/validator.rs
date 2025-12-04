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
    metrics: Option<Arc<crate::utils::metrics::Metrics>>,
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
            metrics: None,
        }
    }

    pub fn with_metrics(mut self, metrics: Arc<crate::utils::metrics::Metrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    pub async fn run(&self) -> Result<()> {
        let mut receiver = self.event_bus.subscribe();
        let mut tasks = JoinSet::new();
        let semaphore = Arc::new(Semaphore::new(4)); // Max 4 parallel validations

        loop {
            tokio::select! {
                event_result = receiver.recv() => {
                    match event_result {
                        Ok(Event::OpportunityFound { opportunity }) => {
                            let permit = semaphore.clone().acquire_owned().await
                                .context("Failed to acquire semaphore permit")?;
                            
                            let event_bus = self.event_bus.clone();
                            let rpc = Arc::clone(&self.rpc);
                            let config = self.config.clone();
                            let balance_manager = Arc::clone(&self.balance_manager);
                            
                            let opportunity_clone = opportunity.clone();
                            let debt_mint = opportunity.debt_mint;
                            let collateral_mint = opportunity.collateral_mint;
                            let max_liquidatable = opportunity.max_liquidatable;
                            let position_address = opportunity.position.address;
                            let metrics = self.metrics.as_ref().map(Arc::clone);
                            
                            tasks.spawn(async move {
                                let _permit = permit;
                                
                                log::debug!(
                                    "Validator: processing opportunity for position {} (est_profit={:.4})",
                                    position_address,
                                    opportunity_clone.estimated_profit
                                );

                                match balance_manager.reserve(&debt_mint, max_liquidatable).await {
                                    Ok(()) => {
                                        // Reserved successfully - proceed with validation
                                    }
                                    Err(e) => {
                                        log::info!(
                                            "Validator: insufficient balance for position {} (debt={}, need={}, available={}): {}",
                                            position_address,
                                            debt_mint,
                                            max_liquidatable,
                                            "checking...",
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
                                        if let Some(ref metrics) = metrics {
                                            metrics.record_opportunity_approved();
                                        }
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
                                        log::info!(
                                            "Validator: opportunity rejected for position {} (debt={}, collateral={}): {}",
                                            position_address,
                                            debt_mint,
                                            collateral_mint,
                                            e
                                        );
                                        if let Some(ref metrics) = metrics {
                                            metrics.record_opportunity_rejected();
                                        }
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

    /// Validate an opportunity using instance methods
    /// This is the public API for validation - can be used by external code
    /// Note: For parallel processing in run(), validate_static is used instead
    pub async fn validate(&self, opp: &Opportunity) -> Result<()> {
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

    async fn validate_static(
        opp: &Opportunity,
        rpc: &Arc<RpcClient>,
        config: &Config,
    ) -> Result<()> {
        log::debug!("🔍 Validating opportunity for position: {}", opp.position.address);

        use tokio::time::{Duration, timeout};
        // ✅ FIX: Total timeout must account for sequential vs parallel oracle checks
        // - Premium RPC: Oracle checks run in parallel → max(RPC timeout) = RPC timeout
        // - Free RPC: Oracle checks run sequentially → RPC timeout + RPC timeout = 2 * RPC timeout
        // Without this fix, free RPC sequential calls (10s + 10s = 20s) would timeout at 12s, causing validation failures
        let rpc_timeout = rpc.request_timeout();
        let total_oracle_timeout = if config.is_free_rpc_endpoint() {
            // Sequential calls: debt oracle (10s) + collateral oracle (10s) + buffer
            // ✅ FIX: Increased buffer from 2s to 5s to handle network delays and prevent timeouts
            // Free RPC sequential checks are more vulnerable to small delays, so larger buffer is safer
            rpc_timeout * 2 + Duration::from_secs(5) // 25s for 10s RPC timeout (was 22s)
        } else {
            // Parallel calls: max(RPC timeout) + buffer
            rpc_timeout + Duration::from_secs(2) // 12s for 10s RPC timeout
        };

        let (debt_result, collateral_result) = timeout(total_oracle_timeout, async {
            if config.is_free_rpc_endpoint() {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                let debt_result = Self::check_oracle_freshness_static(&opp.debt_mint, rpc, config, true).await;
                let collateral_result = Self::check_oracle_freshness_static(&opp.collateral_mint, rpc, config, true).await;
                (debt_result, collateral_result)
            } else {
                let debt_oracle_task = Self::check_oracle_freshness_static(&opp.debt_mint, rpc, config, false);
                let collateral_oracle_task = Self::check_oracle_freshness_static(&opp.collateral_mint, rpc, config, false);
                tokio::join!(debt_oracle_task, collateral_oracle_task)
            }
        }).await.map_err(|_| anyhow::anyhow!(
            "Total oracle check timeout after {:?} (debt + collateral checks exceeded time limit, RPC timeout: {:?})",
            total_oracle_timeout,
            rpc_timeout
        ))?;

        if let Err(e) = debt_result {
            log::debug!("   ❌ Debt oracle FAILED: {}", e);
            return Err(e);
        }

        if let Err(e) = collateral_result {
            log::debug!("   ❌ Collateral oracle FAILED: {}", e);
            return Err(e);
        }

        if let Err(e) = Self::verify_ata_exists_static(&opp.debt_mint, rpc, config).await {
            log::debug!("   ⚠️  Debt ATA verification warning: {} (continuing - ATA can be auto-created)", e);
        }
        if let Err(e) = Self::verify_ata_exists_static(&opp.collateral_mint, rpc, config).await {
            log::debug!("   ⚠️  Collateral ATA verification warning: {} (continuing - ATA can be auto-created)", e);
        }

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

    pub async fn has_sufficient_balance(&self, mint: &Pubkey, amount: u64) -> Result<()> {
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

    pub async fn verify_ata_exists(&self, mint: &Pubkey) -> Result<()> {
        Self::verify_ata_exists_static(mint, &self.rpc, &self.config).await
    }

    async fn verify_ata_exists_static(
        mint: &Pubkey,
        rpc: &Arc<RpcClient>,
        config: &Config,
    ) -> Result<()> {
        let wallet_pubkey_str = config.test_wallet_pubkey
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or("11111111111111111111111111111111");
        let wallet_pubkey = wallet_pubkey_str.parse::<Pubkey>()
            .map_err(|_| anyhow::anyhow!("Invalid wallet pubkey"))?;

        let ata = get_associated_token_address(&wallet_pubkey, mint, Some(config))
            .context("Failed to derive ATA address")?;

        log::debug!(
            "Validator: checking ATA existence: owner_wallet={}, mint={}, ata={}",
            wallet_pubkey,
            mint,
            ata
        );

        match rpc.get_account(&ata).await {
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

    pub async fn check_oracle_freshness(&self, mint: &Pubkey) -> Result<()> {
        Self::check_oracle_freshness_static(mint, &self.rpc, &self.config, false).await
    }

    async fn check_oracle_freshness_static(mint: &Pubkey, rpc: &Arc<RpcClient>, config: &Config, skip_delay: bool) -> Result<()> {
        use crate::protocol::oracle::{get_pyth_oracle_account, read_pyth_price};
        use tokio::time::{Duration, timeout};

        // ✅ CRITICAL FIX: Oracle timeout MUST equal RPC timeout to prevent double-waiting
        // Problem: If oracle timeout < RPC timeout, we wait for oracle timeout, then RPC timeout triggers
        //   Example: oracle timeout (5s) → RPC timeout (10s) → Total wait: 15s ❌
        // Solution: Set oracle timeout = RPC timeout so we only wait once
        //   Example: oracle timeout (10s) = RPC timeout (10s) → Total wait: 10s ✅
        // This prevents cascading timeouts and reduces validation latency
        let oracle_check_timeout = rpc.request_timeout();

        let timeout_result = timeout(oracle_check_timeout, async {
            if config.is_free_rpc_endpoint() && !skip_delay {
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

            let max_age = config.max_oracle_age_seconds;
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
            .await;

        timeout_result.map_err(|_| anyhow::anyhow!(
            "Oracle check timeout after {:?} for mint {}",
            oracle_check_timeout,
            mint
        ))??;

        Ok(())
    }

    pub async fn get_realtime_slippage(&self, opp: &Opportunity) -> Result<u16> {
        Self::get_realtime_slippage_static(opp, &self.rpc, &self.config).await
    }

    async fn get_realtime_slippage_static(opp: &Opportunity, _rpc: &Arc<RpcClient>, config: &Config) -> Result<u16> {
        use crate::strategy::slippage_estimator::SlippageEstimator;

        let estimator = SlippageEstimator::new(config.clone());

        let base_slippage = estimator.estimate_dex_slippage(
            opp.debt_mint,
            opp.collateral_mint,
            opp.seizable_collateral,
        ).await
            .context("Failed to estimate real-time slippage")?;

        // ✅ FIX: Read oracle confidence for BOTH debt and collateral mints
        // Swap involves both input (debt) and output (collateral), each has oracle uncertainty
        let debt_confidence = estimator.read_oracle_confidence(opp.debt_mint).await
            .context("Failed to read debt mint oracle confidence")?;
        let collateral_confidence = estimator.read_oracle_confidence(opp.collateral_mint).await
            .context("Failed to read collateral mint oracle confidence")?;

        // Combine base slippage and both oracle confidences using quadratic sum
        // This is statistically more accurate for combining independent uncertainties
        // than simple addition, which can lead to overly conservative estimates
        // Formula: sqrt(base_slippage^2 + debt_confidence^2 + collateral_confidence^2)
        let total_slippage = {
            let base = base_slippage as f64;
            let debt_conf = debt_confidence as f64;
            let coll_conf = collateral_confidence as f64;
            let sum_squares = base * base + debt_conf * debt_conf + coll_conf * coll_conf;
            (sum_squares.sqrt()) as u16
        };

        // Cap at max_slippage_bps
        let final_slippage = total_slippage.min(config.max_slippage_bps);

        log::debug!(
            "Validator: slippage breakdown for position {}: base={} bps, debt_oracle_conf={} bps, collateral_oracle_conf={} bps, total={} bps (max {} bps)",
            opp.position.address,
            base_slippage,
            debt_confidence,
            collateral_confidence,
            final_slippage,
            config.max_slippage_bps
        );

        Ok(final_slippage)
    }
}
