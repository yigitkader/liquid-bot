pub mod solend;
pub mod oracle;

// Protocol trait
use crate::core::types::{Position, Opportunity};
use solana_sdk::{pubkey::Pubkey, instruction::Instruction};
use async_trait::async_trait;
use std::sync::Arc;
use crate::blockchain::rpc_client::RpcClient;
use anyhow::Result;

// Re-export Protocol trait
#[async_trait]
pub trait Protocol: Send + Sync {
    fn id(&self) -> &str;
    fn program_id(&self) -> Pubkey;
    
    async fn parse_position(&self, account: &solana_sdk::account::Account) -> Option<Position>;
    fn calculate_health_factor(&self, position: &Position) -> f64;
    async fn build_liquidation_ix(
        &self,
        opportunity: &Opportunity,
        liquidator: &Pubkey,
        rpc: Option<Arc<RpcClient>>,
    ) -> Result<Instruction>;
    
    fn liquidation_params(&self) -> LiquidationParams;
}

#[derive(Debug, Clone)]
pub struct LiquidationParams {
    pub bonus: f64,
    pub close_factor: f64,
    pub max_slippage: f64,
}

