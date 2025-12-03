pub mod accounts;
pub mod instructions;
pub mod types;

use crate::blockchain::rpc_client::RpcClient;
use crate::core::config::Config;
use crate::core::types::{Opportunity, Position};
use crate::protocol::{LiquidationParams, Protocol};
use anyhow::Result;
use async_trait::async_trait;
use solana_sdk::{account::Account, instruction::Instruction, pubkey::Pubkey};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub struct SolendProtocol {
    program_id: Pubkey,
    config: Config,
}

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
        use crate::core::types::{Asset, Position};
        use crate::protocol::solend::types::SolendObligation;
        use log;

        let data_len = account.data.len();

        const DISCRIMINATOR_SIZE: usize = 8;
        if data_len < DISCRIMINATOR_SIZE {
            return None; // Can't even read discriminator, skip immediately
        }

        let account_discriminator = &account.data[0..DISCRIMINATOR_SIZE];
        let expected_discriminator = get_obligation_discriminator();

        if account_discriminator != expected_discriminator {
            return None; // Wrong account type, skip immediately (before expensive size check)
        }

        const MIN_OBLIGATION_SIZE: usize = 1200;
        const MAX_OBLIGATION_SIZE: usize = 1500;

        if data_len < MIN_OBLIGATION_SIZE || data_len > MAX_OBLIGATION_SIZE {
            return None; // Wrong account type, skip immediately
        }

        let obligation = match SolendObligation::from_account_data(&account.data) {
            Ok(obl) => obl,
            Err(e) => {
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

        if obligation.deposits.is_empty() && obligation.borrows.is_empty() {
            return None;
        }

        let health_factor = obligation.calculate_health_factor();
        let skip_threshold =
            self.config.hf_liquidation_threshold * self.config.liquidation_safety_margin;
        // ✅ FIX: Use >= to match analyzer's < logic exactly
        // Analyzer liquidates if HF < threshold, so parser should skip if HF >= threshold
        // This prevents wasting CPU parsing positions that analyzer won't liquidate
        if health_factor >= skip_threshold {
            return None;
        }

        let collateral_usd = obligation.total_deposited_value_usd();
        let debt_usd = obligation.total_borrowed_value_usd();

        // Zero-value obligation check (after health factor, but still useful)
        if collateral_usd < 0.01 && debt_usd < 0.01 {
            return None;
        }

        let logged = LOGGED_OBLIGATIONS.fetch_add(1, Ordering::Relaxed);

        if logged < 5 {
            log::debug!(
                "🧩 Solend Obligation Parsed: owner={}, hf={:.6}, deposited=${:.2}, borrowed=${:.2}, deposits={}, borrows={}",
                obligation.owner,
                health_factor,
                collateral_usd,
                debt_usd,
                obligation.deposits.len(),
                obligation.borrows.len()
            );
        }

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

// ✅ FIX: Pre-computed discriminator to avoid SHA256 calculation on every call
// SHA256("account:Obligation")[..8] = [0xa8, 0xce, 0x8d, 0x6a, 0x58, 0x4c, 0xac, 0xa7]
const OBLIGATION_DISCRIMINATOR: [u8; 8] = [0xa8, 0xce, 0x8d, 0x6a, 0x58, 0x4c, 0xac, 0xa7];

fn get_obligation_discriminator() -> [u8; 8] {
    OBLIGATION_DISCRIMINATOR
}
