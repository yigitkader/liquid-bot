use crate::core::events::{Event, EventBus};
use crate::core::types::Opportunity;
use crate::core::config::Config;
use crate::blockchain::rpc_client::RpcClient;
use crate::strategy::balance_manager::BalanceManager;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::broadcast;
use solana_sdk::pubkey::Pubkey;
// use spl_associated_token_account::get_associated_token_address; // Would need spl-token dependency

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
                Ok(_) => {
                    // Ignore other events
                }
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
        // 1. Check balance
        self.has_sufficient_balance(&opp.debt_mint, opp.max_liquidatable).await?;

        // 2. Check oracle price (simplified - would need oracle implementation)
        // check_oracle_freshness(&opp.debt_mint)?;
        // check_oracle_freshness(&opp.collateral_mint)?;

        // 3. Verify token accounts exist
        self.verify_ata_exists(&opp.debt_mint).await?;
        self.verify_ata_exists(&opp.collateral_mint).await?;

        // 4. Re-check slippage (simplified - would need slippage estimator)
        // let slippage = self.get_realtime_slippage(opp).await?;
        // if slippage > self.config.max_slippage_bps {
        //     return Err(anyhow::anyhow!("Slippage too high"));
        // }

        // 5. Lock balance (prevent double-spending)
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

    async fn verify_ata_exists(&self, _mint: &Pubkey) -> Result<()> {
        // Simplified - would need to check if ATA exists on-chain
        // For now, just return Ok
        Ok(())
    }
}
