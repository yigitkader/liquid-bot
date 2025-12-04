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

                                let max_liquidatable_token_amount = match crate::utils::helpers::convert_usd_to_token_amount_for_mint(
                                    &debt_mint,
                                    max_liquidatable,
                                    &rpc,
                                    &config,
                                ).await {
                                    Ok(amount) => amount,
                                    Err(e) => {
                                        log::warn!("Validator: Failed to convert max_liquidatable from USD to token amount for position {}: {}", position_address, e);
                                        return;
                                    }
                                };
                                
                                log::debug!(
                                    "Validator: Converted max_liquidatable: USD×1e6={}, token_amount={} for debt_mint={}",
                                    max_liquidatable,
                                    max_liquidatable_token_amount,
                                    debt_mint
                                );

                                match balance_manager.reserve(&debt_mint, max_liquidatable_token_amount).await {
                                    Ok(()) => {}
                                    Err(e) => {
                                        log::info!("Validator: insufficient balance for position {} (debt={}, need_token={}, need_usd={}): {}", position_address, debt_mint, max_liquidatable_token_amount, max_liquidatable, e);
                                        return;
                                    }
                                }
                                
                                match Self::validate_static(&opportunity_clone, &rpc, &config).await {
                                    Ok(()) => {
                                        log::info!("Validator: opportunity approved for position {}", position_address);
                                        if let Some(ref metrics) = metrics {
                                            metrics.record_opportunity_approved();
                                        }
                                        if let Err(e) = event_bus.publish(Event::OpportunityApproved {
                                            opportunity: opportunity_clone,
                                        }) {
                                            log::error!("Failed to publish OpportunityApproved: {}", e);
                                            balance_manager.release(&debt_mint, max_liquidatable_token_amount).await;
                                        }
                                    }
                                    Err(e) => {
                                        log::info!("Validator: opportunity rejected for position {} (debt={}, collateral={}): {}", position_address, debt_mint, collateral_mint, e);
                                        if let Some(ref metrics) = metrics {
                                            metrics.record_opportunity_rejected();
                                        }
                                        balance_manager.release(&debt_mint, max_liquidatable_token_amount).await;
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

    pub async fn validate(&self, opp: &Opportunity) -> Result<()> {
        let max_liquidatable_token_amount = crate::utils::helpers::convert_usd_to_token_amount_for_mint(
            &opp.debt_mint,
            opp.max_liquidatable,
            &self.rpc,
            &self.config,
        )
        .await
        .context("Failed to convert max_liquidatable from USD to token amount")?;
        
        self.balance_manager
            .reserve(&opp.debt_mint, max_liquidatable_token_amount)
            .await
            .context("Insufficient balance - reservation failed")?;

        match Self::validate_static(opp, &self.rpc, &self.config).await {
            Ok(()) => Ok(()),
            Err(e) => {
                self.balance_manager.release(&opp.debt_mint, max_liquidatable_token_amount).await;
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
        let rpc_timeout = rpc.request_timeout();
        let total_oracle_timeout = if config.is_free_rpc_endpoint() {
            rpc_timeout * 2 + Duration::from_secs(5)
        } else {
            rpc_timeout + Duration::from_secs(2)
        };

        let (debt_result, collateral_result) = timeout(total_oracle_timeout, async {
            if config.is_free_rpc_endpoint() {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                let debt = Self::check_oracle_freshness_static(&opp.debt_mint, rpc, config, true).await;
                let collateral = Self::check_oracle_freshness_static(&opp.collateral_mint, rpc, config, true).await;
                (debt, collateral)
            } else {
                tokio::join!(
                    Self::check_oracle_freshness_static(&opp.debt_mint, rpc, config, false),
                    Self::check_oracle_freshness_static(&opp.collateral_mint, rpc, config, false)
                )
            }
        }).await.map_err(|_| anyhow::anyhow!(
            "Oracle check timeout after {:?} (RPC timeout: {:?})",
            total_oracle_timeout, rpc_timeout
        ))?;

        debt_result.map_err(|e| {
            log::debug!("Debt oracle failed: {}", e);
            e
        })?;
        collateral_result.map_err(|e| {
            log::debug!("Collateral oracle failed: {}", e);
            e
        })?;

        let _ = Self::verify_ata_exists_static(&opp.debt_mint, rpc, config).await;
        let _ = Self::verify_ata_exists_static(&opp.collateral_mint, rpc, config).await;

        let slippage = Self::get_realtime_slippage_static(opp, rpc, config).await
            .map_err(|e| {
                log::debug!("Slippage calculation failed: {}", e);
                e
            })?;
        
        if slippage > config.max_slippage_bps {
            return Err(anyhow::anyhow!("Slippage too high: {} bps (max: {})", slippage, config.max_slippage_bps));
        }

        log::debug!("Validation passed for position: {}", opp.position.address);
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
                        log::warn!("ATA exists but is empty: {}", ata);
                        return Ok(());
                    }
                    log::debug!("Validator: ATA exists and non-empty: owner_wallet={}, mint={}, ata={}, data_len={}", wallet_pubkey, mint, ata, account.data.len());
                    Ok(())
                }
                Err(e) => {
                    log::warn!("ATA not found for mint {} ({}), will be auto-created if needed", mint, ata);
                    log::warn!("Error: {}", e);
                    Ok(())
                }
            }
    }

    pub async fn check_oracle_freshness(&self, mint: &Pubkey) -> Result<()> {
        Self::check_oracle_freshness_static(mint, &self.rpc, &self.config, false).await
    }

    async fn check_oracle_freshness_static(mint: &Pubkey, rpc: &Arc<RpcClient>, config: &Config, skip_delay: bool) -> Result<()> {
        use crate::protocol::oracle::{get_pyth_oracle_account, get_switchboard_oracle_account};
        use tokio::time::{Duration, timeout};

        let oracle_check_timeout = rpc.request_timeout();

        let timeout_result = timeout(oracle_check_timeout, async {
            if config.is_free_rpc_endpoint() && !skip_delay {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            let pyth_account = get_pyth_oracle_account(mint, Some(config)).ok().flatten();
            let switchboard_account = get_switchboard_oracle_account(mint, Some(config)).ok().flatten();

            if pyth_account.is_none() && switchboard_account.is_none() {
                log::warn!("No oracle found (Pyth or Switchboard) for mint: {}, skipping freshness check", mint);
                return Err(anyhow::anyhow!("No oracle found for mint {}", mint));
            }

            use crate::protocol::oracle::read_oracle_price;
            
            let oracle_type = if pyth_account.is_some() { "Pyth" } else { "Switchboard" };
            let oracle_account = pyth_account.or(switchboard_account).unwrap();
            
            let price_data = match read_oracle_price(
                pyth_account.as_ref(),
                switchboard_account.as_ref(),
                Arc::clone(rpc),
                Some(config),
            ).await {
                Ok(Some(data)) => data,
                Ok(None) => {
                    return Err(anyhow::anyhow!(
                        "Failed to read oracle price data for mint {} ({} oracle={}) - price data is None",
                        mint, oracle_type, oracle_account
                    ));
                }
                Err(e) => {
                    let error_str = e.to_string().to_lowercase();
                    if error_str.contains("timeout") || error_str.contains("timed out") {
                        log::error!("Validator: {} oracle read timeout for mint {} (oracle={}) - RPC may be slow or unresponsive: {}", oracle_type, mint, oracle_account, e);
                        return Err(anyhow::anyhow!("{} oracle read timeout for mint {} (oracle={}) - RPC may be slow or unresponsive: {}", oracle_type, mint, oracle_account, e));
                    } else {
                        log::error!("Validator: failed to read {} price for mint {} (oracle={}): {}", oracle_type, mint, oracle_account, e);
                        return Err(anyhow::anyhow!("Failed to read oracle price data for mint {} ({} oracle={}): {}", mint, oracle_type, oracle_account, e));
                    }
                }
            };


            Self::validate_price_data(mint, &price_data, oracle_type, &oracle_account, config.max_oracle_age_seconds)?;

            let current_time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| anyhow::anyhow!("Failed to get current time: {}", e))?
                .as_secs() as i64;
            let age_seconds = current_time - price_data.timestamp;

            log::debug!(
                "Validator: {} oracle price is valid and fresh for mint {} (oracle={}, age={}s, max_age={}s, price={:.4}, confidence={:.4}, conf_ratio={:.2}%)",
                oracle_type,
                mint,
                oracle_account,
                age_seconds,
                config.max_oracle_age_seconds,
                price_data.price,
                price_data.confidence,
                if price_data.price > 0.0 { (price_data.confidence / price_data.price) * 100.0 } else { 0.0 }
            );

            Ok(())
        }).await;

        timeout_result.map_err(|_| anyhow::anyhow!("Oracle check timeout after {:?} for mint {}", oracle_check_timeout, mint))??;
        Ok(())
    }


    fn validate_price_data(
        mint: &Pubkey,
        price_data: &crate::protocol::oracle::OraclePrice,
        oracle_type: &str,
        _oracle_account: &Pubkey,
        max_age: u64,
    ) -> Result<()> {
        const MAX_PRICE: f64 = 1_000_000_000_000.0;
        const MIN_PRICE: f64 = 0.0001;
        const MAX_CONF_RATIO: f64 = 0.25;

        let age = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| anyhow::anyhow!("Time error: {}", e))?
            .as_secs() as i64 - price_data.timestamp;

        if age > max_age as i64 {
            return Err(anyhow::anyhow!("{} oracle stale for {}: {}s old (max: {}s)", oracle_type, mint, age, max_age));
        }
        if price_data.price <= 0.0 {
            return Err(anyhow::anyhow!("{} oracle invalid price for {}: {:.4}", oracle_type, mint, price_data.price));
        }
        if !(MIN_PRICE..=MAX_PRICE).contains(&price_data.price) {
            return Err(anyhow::anyhow!("{} oracle price out of range for {}: {:.4}", oracle_type, mint, price_data.price));
        }
        if price_data.confidence < 0.0 {
            return Err(anyhow::anyhow!("{} oracle negative confidence for {}: {:.4}", oracle_type, mint, price_data.confidence));
        }
        if price_data.price > 0.0 {
            let conf_ratio = price_data.confidence / price_data.price;
            if conf_ratio > MAX_CONF_RATIO {
                return Err(anyhow::anyhow!("{} oracle confidence too high for {}: {:.2}% (max: {:.0}%)", oracle_type, mint, conf_ratio * 100.0, MAX_CONF_RATIO * 100.0));
            }
        }
        Ok(())
    }

    pub async fn get_realtime_slippage(&self, opp: &Opportunity) -> Result<u16> {
        Self::get_realtime_slippage_static(opp, &self.rpc, &self.config).await
    }

    async fn get_realtime_slippage_static(opp: &Opportunity, rpc: &Arc<RpcClient>, config: &Config) -> Result<u16> {
        use crate::strategy::slippage_estimator::SlippageEstimator;

        let estimator = SlippageEstimator::new(config.clone());

        let max_liquidatable_token_amount = crate::utils::helpers::convert_usd_to_token_amount_for_mint(
            &opp.debt_mint,
            opp.max_liquidatable,
            rpc,
            config,
        )
        .await
            .context("Failed to convert max_liquidatable from USD to token amount for slippage estimation")?;
        
        log::debug!("get_realtime_slippage_static: Converted max_liquidatable for slippage: USD×1e6={}, token_amount={} for debt_mint={}", opp.max_liquidatable, max_liquidatable_token_amount, opp.debt_mint);

        let base_slippage = estimator.estimate_dex_slippage(
            opp.debt_mint,
            opp.collateral_mint,
            max_liquidatable_token_amount,
        ).await
            .context("Failed to estimate real-time slippage")?;

        let debt_confidence = estimator.read_oracle_confidence(opp.debt_mint).await
            .context("Failed to read debt mint oracle confidence")?;
        let collateral_confidence = estimator.read_oracle_confidence(opp.collateral_mint).await
            .context("Failed to read collateral mint oracle confidence")?;

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
