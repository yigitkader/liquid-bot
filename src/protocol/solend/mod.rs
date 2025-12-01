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
use std::sync::atomic::{AtomicUsize, Ordering};
use anyhow::Result;
use std::str::FromStr;

pub struct SolendProtocol {
    program_id: Pubkey,
    config: Config,
}

// Sadece sınırlı sayıda obligation'ı derinlemesine loglamak için
static LOGGED_OBLIGATIONS: AtomicUsize = AtomicUsize::new(0);

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
        use crate::protocol::solend::types::SolendObligation;
        use crate::core::types::{Position, Asset};
        use log;

        let data_len = account.data.len();

        // FAST PATH: Size check önce
        const EXPECTED_OBLIGATION_SIZE: usize = 1300;
        const MIN_OBLIGATION_SIZE: usize = 1200; // Biraz tolerans
        
        if data_len < MIN_OBLIGATION_SIZE {
            // Bu çok küçük - muhtemelen closed obligation
            log::debug!(
                "SolendProtocol: skipping account (too small): {} bytes (expected: {})",
                data_len,
                EXPECTED_OBLIGATION_SIZE
            );
            return None;
        }
        
        if data_len > EXPECTED_OBLIGATION_SIZE + 100 {
            // Bu çok büyük - muhtemelen farklı account tipi
            log::debug!(
                "SolendProtocol: skipping account (too large): {} bytes (expected: {})",
                data_len,
                EXPECTED_OBLIGATION_SIZE
            );
            return None;
        }
        
        // Obligation parse et
        let obligation = match SolendObligation::from_account_data(&account.data) {
            Ok(obl) => obl,
            Err(e) => {
                // Parse error - log sadece ilk birkaç kez (spam önlemek için)
                static PARSE_ERROR_COUNT: AtomicUsize = AtomicUsize::new(0);
                
                let count = PARSE_ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
                
                if count < 10 {
                    log::debug!(
                        "SolendProtocol: failed to parse obligation (data_len={}): {}",
                        data_len,
                        e
                    );
                }
                
                return None;
            }
        };
        
        // Empty obligation check
        if obligation.deposits.is_empty() && obligation.borrows.is_empty() {
            log::debug!(
                "SolendProtocol: skipping empty obligation (owner={})",
                obligation.owner
            );
            return None;
        }
        
        // Zero-value obligation check
        let deposited = obligation.total_deposited_value_usd();
        let borrowed = obligation.total_borrowed_value_usd();
        
        if deposited < 0.01 && borrowed < 0.01 {
            log::debug!(
                "SolendProtocol: skipping near-zero obligation (owner={}, deposited=${:.4}, borrowed=${:.4})",
                obligation.owner,
                deposited,
                borrowed
            );
            return None;
        }

        // Log detaylı info (sadece ilk birkaç kez)
        let logged = LOGGED_OBLIGATIONS.fetch_add(1, Ordering::Relaxed);
        
        if logged < 5 {
            let health_factor = obligation.calculate_health_factor();
            log::info!(
                "🧩 Solend Obligation Parsed: owner={}, hf={:.6}, deposited=${:.2}, borrowed=${:.2}, deposits={}, borrows={}",
                obligation.owner,
                health_factor,
                deposited,
                borrowed,
                obligation.deposits.len(),
                obligation.borrows.len()
            );
        }

        let health_factor = obligation.calculate_health_factor();
        let collateral_usd = obligation.total_deposited_value_usd();
        let debt_usd = obligation.total_borrowed_value_usd();

        let mut collateral_assets = Vec::new();
        for deposit in &obligation.deposits {
            collateral_assets.push(Asset {
                mint: deposit.deposit_reserve,
                amount: deposit.deposited_amount,
                amount_usd: deposit.market_value.to_f64(),
                ltv: 0.0,
            });
        }

        let mut debt_assets = Vec::new();
        for borrow in &obligation.borrows {
            let borrowed_amount = borrow.borrowed_amount_wads.to_f64() as u64;
            debt_assets.push(Asset {
                mint: borrow.borrow_reserve,
                amount: borrowed_amount,
                amount_usd: borrow.market_value.to_f64(),
                ltv: 0.0,
            });
        }

        Some(Position {
            address: obligation.owner,
            health_factor,
            collateral_usd,
            debt_usd,
            collateral_assets,
            debt_assets,
        })
    }

    fn calculate_health_factor(&self, position: &Position) -> f64 {
        position.health_factor
    }

    async fn build_liquidation_ix(
        &self,
        opportunity: &Opportunity,
        liquidator: &Pubkey,
        rpc: Option<Arc<RpcClient>>,
    ) -> Result<Instruction> {
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
