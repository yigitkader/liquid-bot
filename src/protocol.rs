use crate::domain::AccountPosition;
use crate::solana_client::SolanaClient;
use anyhow::Result;
use async_trait::async_trait;
use solana_sdk::{account::Account, instruction::Instruction, pubkey::Pubkey};
use std::sync::Arc;

#[async_trait]
pub trait Protocol: Send + Sync {
    fn id(&self) -> &str;
    fn program_id(&self) -> Pubkey;
    async fn parse_account_position(
        &self,
        account_address: &Pubkey,
        account_data: &Account,
        rpc_client: Option<Arc<SolanaClient>>,
    ) -> Result<Option<AccountPosition>>;
    fn calculate_health_factor(&self, position: &AccountPosition) -> Result<f64>;
    fn get_liquidation_params(&self) -> LiquidationParams;
    async fn build_liquidation_instruction(
        &self,
        opportunity: &crate::domain::LiquidationOpportunity,
        liquidator: &Pubkey,
        rpc_client: Option<Arc<SolanaClient>>,
    ) -> Result<Instruction>;
}

#[derive(Debug, Clone)]
pub struct LiquidationParams {
    pub liquidation_bonus: f64,
    pub close_factor: f64,
    pub max_liquidation_slippage: f64,
}
