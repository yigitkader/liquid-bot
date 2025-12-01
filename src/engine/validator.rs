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
        log::debug!(
            "Validator: starting validation for position {}",
            opp.position.address
        );

        self.has_sufficient_balance(&opp.debt_mint, opp.max_liquidatable).await?;
        
        self.check_oracle_freshness(&opp.debt_mint).await?;
        self.check_oracle_freshness(&opp.collateral_mint).await?;
        
        self.verify_ata_exists(&opp.debt_mint).await?;
        self.verify_ata_exists(&opp.collateral_mint).await?;
        
        let slippage = self.get_realtime_slippage(opp).await?;
        if slippage > self.config.max_slippage_bps {
            return Err(anyhow::anyhow!("Slippage too high: {} bps (max: {} bps)", slippage, self.config.max_slippage_bps));
        }

        log::debug!(
            "Validator: reserving balance for mint {} amount {}",
            opp.debt_mint,
            opp.max_liquidatable
        );
        
        self.balance_manager.reserve(&opp.debt_mint, opp.max_liquidatable).await?;
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
        let ata = get_associated_token_address(&wallet_pubkey, mint, None)?;
        match self.rpc.get_account(&ata).await {
            Ok(account) => {
                if account.data.is_empty() {
                    return Err(anyhow::anyhow!(
                        "Associated token account {} for mint {} is empty",
                        ata,
                        mint
                    ));
                }
                log::debug!(
                    "Validator: ATA exists and non-empty: owner_wallet={}, mint={}, ata={}",
                    wallet_pubkey,
                    mint,
                    ata
                );
                Ok(())
            }
            Err(e) => {
                log::warn!(
                    "Validator: failed to fetch ATA {} for mint {} (wallet {}): {} - treating as non-fatal",
                    ata,
                    mint,
                    wallet_pubkey,
                    e
                );
                Ok(())
            }
        }
    }

    async fn check_oracle_freshness(&self, mint: &Pubkey) -> Result<()> {
        use crate::protocol::oracle::{get_pyth_oracle_account, read_pyth_price};
        
        if let Some(oracle_account) = get_pyth_oracle_account(mint, Some(&self.config))? {
            log::debug!(
                "Validator: checking oracle freshness for mint {} (oracle={})",
                mint,
                oracle_account
            );
            let max_age = self.config.max_oracle_age_seconds;
            if let Some(price_data) = read_pyth_price(&oracle_account, Arc::clone(&self.rpc), Some(&self.config)).await? {
                let current_time = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| anyhow::anyhow!("Failed to get current time: {}", e))?
                    .as_secs() as i64;
                
                let age_seconds = current_time - price_data.timestamp;
                if age_seconds > max_age as i64 {
                    return Err(anyhow::anyhow!(
                        "Oracle price data is stale for mint {} (oracle {}): {} seconds old (max: {} seconds)",
                        mint,
                        oracle_account,
                        age_seconds,
                        max_age
                    ));
                }
                log::debug!(
                    "Validator: oracle price is fresh for mint {} (oracle={}, age={}s, max_age={}s)",
                    mint,
                    oracle_account,
                    age_seconds,
                    max_age
                );
            } else {
                return Err(anyhow::anyhow!(
                    "Failed to read oracle price data for mint {} (oracle={})",
                    mint,
                    oracle_account
                ));
            }
        } else {
            log::warn!("No Pyth oracle found for mint: {}, skipping freshness check", mint);
        }
        
        Ok(())
    }

    async fn get_realtime_slippage(&self, opp: &Opportunity) -> Result<u16> {
        use crate::strategy::slippage_estimator::SlippageEstimator;
        
        let estimator = SlippageEstimator::new(self.config.clone());
        let slippage = estimator.estimate_dex_slippage(
            opp.debt_mint,
            opp.collateral_mint,
            opp.seizable_collateral,
        ).await
            .context("Failed to estimate real-time slippage")?;

        log::debug!(
            "Validator: real-time slippage for position {} is {} bps (max {} bps)",
            opp.position.address,
            slippage,
            self.config.max_slippage_bps
        );
        
        Ok(slippage)
    }
}
