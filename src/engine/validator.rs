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
                    if self.validate(&opportunity).await.is_ok() {
                        self.event_bus.publish(Event::OpportunityApproved {
                            opportunity,
                        })?;
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
        self.has_sufficient_balance(&opp.debt_mint, opp.max_liquidatable).await?;
        
        self.check_oracle_freshness(&opp.debt_mint).await?;
        self.check_oracle_freshness(&opp.collateral_mint).await?;
        
        self.verify_ata_exists(&opp.debt_mint).await?;
        self.verify_ata_exists(&opp.collateral_mint).await?;
        
        let slippage = self.get_realtime_slippage(opp).await?;
        if slippage > self.config.max_slippage_bps {
            return Err(anyhow::anyhow!("Slippage too high: {} bps (max: {} bps)", slippage, self.config.max_slippage_bps));
        }
        
        self.balance_manager.reserve(&opp.debt_mint, opp.max_liquidatable).await?;
        Ok(())
    }

    async fn has_sufficient_balance(&self, mint: &Pubkey, amount: u64) -> Result<()> {
        let available = self.balance_manager.get_available_balance(mint).await?;
        if available < amount {
            return Err(anyhow::anyhow!("Insufficient balance: need {}, have {}", amount, available));
        }
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
                    return Err(anyhow::anyhow!("Associated token account is empty"));
                }
                Ok(())
            }
            Err(_) => Ok(())
        }
    }

    async fn check_oracle_freshness(&self, mint: &Pubkey) -> Result<()> {
        use crate::protocol::oracle::{get_pyth_oracle_account, read_pyth_price};
        
        if let Some(oracle_account) = get_pyth_oracle_account(mint, Some(&self.config))? {
            let max_age = self.config.max_oracle_age_seconds;
            if let Some(price_data) = read_pyth_price(&oracle_account, Arc::clone(&self.rpc), Some(&self.config)).await? {
                let current_time = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| anyhow::anyhow!("Failed to get current time: {}", e))?
                    .as_secs() as i64;
                
                let age_seconds = current_time - price_data.timestamp;
                if age_seconds > max_age as i64 {
                    return Err(anyhow::anyhow!("Oracle price data is stale: {} seconds old (max: {} seconds)", age_seconds, max_age));
                }
            } else {
                return Err(anyhow::anyhow!("Failed to read oracle price data for mint: {}", mint));
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
        
        Ok(slippage)
    }
}
