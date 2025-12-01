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
        let obligation = match SolendObligation::from_account_data(&account.data) {
            Ok(obl) => obl,
            Err(e) => {
                log::debug!(
                    "SolendProtocol: failed to parse obligation account (data_len={}): {}",
                    data_len,
                    e
                );
                return None;
            }
        };

        let deposits_len = obligation.deposits.len();
        let borrows_len = obligation.borrows.len();

        // Sadece ilk birkaç obligation için derin log (log dosyasını şişirmemek için)
        let logged = LOGGED_OBLIGATIONS.fetch_add(1, Ordering::Relaxed);
        if logged < 5 {
            let deposited_usd = obligation.total_deposited_value_usd();
            let borrowed_usd = obligation.total_borrowed_value_usd();
            let health_factor = obligation.calculate_health_factor();

            log::info!(
                "🧩 Solend Obligation Parsed (runtime): data_len={}, health_factor={:.6}, deposited_usd={:.6}, borrowed_usd={:.6}, deposits_len={}, borrows_len={}",
                data_len,
                health_factor,
                deposited_usd,
                borrowed_usd,
                deposits_len,
                borrows_len
            );

            for (i, dep) in obligation.deposits.iter().enumerate() {
                log::info!(
                    "   ▸ Deposit[{}]: reserve={}, deposited_amount={}, market_value_raw={}, market_value_usd={:.6}",
                    i,
                    dep.deposit_reserve,
                    dep.deposited_amount,
                    dep.market_value.value,
                    dep.market_value.to_f64()
                );
            }

            for (i, bor) in obligation.borrows.iter().enumerate() {
                log::info!(
                    "   ▸ Borrow[{}]: reserve={}, borrowed_amount_wad={}, market_value_raw={}, market_value_usd={:.6}",
                    i,
                    bor.borrow_reserve,
                    bor.borrowed_amount_wad,
                    bor.market_value.value,
                    bor.market_value.to_f64()
                );
            }
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
            let borrowed_amount = (borrow.borrowed_amount_wad as f64 / 1_000_000_000_000_000_000.0) as u64;
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
