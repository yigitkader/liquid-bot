use crate::core::events::{Event, EventBus};
use crate::core::types::Opportunity;
use crate::core::config::Config;
use crate::blockchain::rpc_client::RpcClient;
use crate::strategy::balance_manager::BalanceManager;
use crate::protocol::solend::accounts::get_associated_token_address;
use anyhow::Result;
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
        self.verify_ata_exists(&opp.debt_mint).await?;
        self.verify_ata_exists(&opp.collateral_mint).await?;
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
        let wallet_pubkey = Pubkey::try_from("11111111111111111111111111111111")
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
}
