pub mod accounts;
pub mod instructions;
pub mod types;

use crate::core::types::{Position, Opportunity};
use crate::core::config::Config;
use crate::protocol::{Protocol, LiquidationParams};
use crate::blockchain::rpc_client::RpcClient;
use solana_sdk::{pubkey::Pubkey, instruction::Instruction, account::Account};
use async_trait::async_trait;
use std::sync::Arc;
use anyhow::Result;
use std::str::FromStr;

pub struct SolendProtocol {
    program_id: Pubkey,
    config: Config,
}

impl SolendProtocol {
    pub fn new(config: &Config) -> Result<Self> {
        let program_id = Pubkey::from_str(&config.solend_program_id)
            .map_err(|e| anyhow::anyhow!("Invalid Solend program ID: {}", e))?;
        
        Ok(SolendProtocol {
            program_id,
            config: config.clone(),
        })
    }
}

#[async_trait]
impl Protocol for SolendProtocol {
    fn id(&self) -> &str {
        "solend"
    }

    fn program_id(&self) -> Pubkey {
        self.program_id
    }

    async fn parse_position(&self, account: &Account) -> Option<Position> {
        // Real implementation required - parse Solend obligation account
        // For now, return None to indicate parsing not yet implemented
        // This must be implemented with real Borsh deserialization
        None
    }

    fn calculate_health_factor(&self, position: &Position) -> f64 {
        // Real implementation required - calculate health factor from position
        // For now, return position's existing health factor
        position.health_factor
    }

    async fn build_liquidation_ix(
        &self,
        opportunity: &Opportunity,
        liquidator: &Pubkey,
        rpc: Option<Arc<RpcClient>>,
    ) -> Result<Instruction> {
        // Use the instruction builder from instructions.rs
        instructions::build_liquidate_obligation_ix(opportunity, liquidator, rpc).await
    }

    fn liquidation_params(&self) -> LiquidationParams {
        LiquidationParams {
            bonus: self.config.liquidation_bonus,
            close_factor: self.config.close_factor,
            max_slippage: self.config.max_liquidation_slippage,
        }
    }
}
