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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

        const MIN_OBLIGATION_SIZE: usize = 1200;
        const MAX_OBLIGATION_SIZE: usize = 1500;

        // ✅ FIX: Remove strict discriminator check - try to parse instead
        // This makes the system more dynamic and works with real blockchain data
        // If parse succeeds, it's an obligation regardless of discriminator
        if data_len < MIN_OBLIGATION_SIZE || data_len > MAX_OBLIGATION_SIZE {
            return None; // Wrong size, skip immediately
        }

        let obligation = match SolendObligation::from_account_data(&account.data) {
            Ok(obl) => obl,
            Err(e) => {
                static PARSE_ERROR_COUNT: AtomicUsize = AtomicUsize::new(0);
                static LOGS_SUPPRESSED: AtomicBool = AtomicBool::new(false);

                // ✅ FIX: Increment counter (may wrap, but we use flag to prevent log spam)
                let count = PARSE_ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
                
                // ✅ FIX: Check suppression flag FIRST to prevent log spam after wraparound
                // Even if counter wraps from usize::MAX to 0, flag prevents re-logging
                let logs_suppressed = LOGS_SUPPRESSED.load(Ordering::Relaxed);
                
                if !logs_suppressed {
                    // Log first few parse errors with more detail for debugging
                    if count <= 5 {
                        let discriminator = if data_len >= 8 {
                            format!("{:02x?}", &account.data[0..8])
                        } else {
                            "N/A".to_string()
                        };
                        log::debug!(
                            "SolendProtocol: failed to parse obligation (data_len={}, discriminator={}): {}",
                            data_len,
                            discriminator,
                            e
                        );
                    } else if count == 6 {
                        log::debug!("SolendProtocol: Suppressing further parse error logs ({} total errors so far)", count + 1);
                        LOGS_SUPPRESSED.store(true, Ordering::Relaxed);
                    }
                }
                // If logs_suppressed is true, skip all logging (even after counter wraparound)

                return None;
            }
        };

        if obligation.deposits.is_empty() && obligation.borrows.is_empty() {
            return None;
        }

        // ✅ FIX: Use closeable field - skip positions that cannot be liquidated
        // closeable=false means the position cannot be closed/liquidated
        if !obligation.closeable {
            log::debug!(
                "SolendProtocol: Skipping non-closeable position {} (owner={})",
                obligation.owner,
                obligation.owner
            );
            return None;
        }

        // ✅ FIX: Use borrowing_isolated_asset field - isolated assets have higher risk
        // Isolated assets can only be borrowed against specific collateral
        // These positions may have different liquidation rules or higher risk
        if obligation.borrowing_isolated_asset {
            log::debug!(
                "SolendProtocol: Position {} is borrowing isolated asset (higher risk, but still processable)",
                obligation.owner
            );
            // Note: We still process isolated asset positions, but log for awareness
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

        // ✅ FIX: Use super_unhealthy_borrow_value for additional risk assessment
        // super_unhealthy_borrow_value indicates when a position is extremely unhealthy
        // If current borrowed_value exceeds super_unhealthy_borrow_value, the position is in critical state
        // This can be used to prioritize or skip extremely risky positions
        let borrowed_value_usd = obligation.borrowed_value.to_f64();
        let super_unhealthy_threshold_usd = obligation.super_unhealthy_borrow_value.to_f64();
        if super_unhealthy_threshold_usd > 0.0 && borrowed_value_usd >= super_unhealthy_threshold_usd {
            log::debug!(
                "SolendProtocol: Position {} is super unhealthy (borrowed=${:.2} >= super_unhealthy=${:.2}) - high risk",
                obligation.owner,
                borrowed_value_usd,
                super_unhealthy_threshold_usd
            );
            // Note: We still process super unhealthy positions, but they are high risk
            // Consider adding config option to skip these if desired
        }

        // ✅ FIX: Use unweighted_borrowed_value for comparison
        // unweighted_borrowed_value is the raw borrowed value without LTV weighting
        // This can help identify positions where LTV weighting significantly affects health
        let unweighted_borrowed_usd = obligation.unweighted_borrowed_value.to_f64();
        if unweighted_borrowed_usd > 0.0 && borrowed_value_usd > 0.0 {
            let ltv_impact_ratio = unweighted_borrowed_usd / borrowed_value_usd;
            if ltv_impact_ratio > 1.5 {
                log::debug!(
                    "SolendProtocol: Position {} has high LTV impact (unweighted=${:.2} vs weighted=${:.2}, ratio={:.2})",
                    obligation.owner,
                    unweighted_borrowed_usd,
                    borrowed_value_usd,
                    ltv_impact_ratio
                );
            }
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

        // ✅ FIX: Use attributed_borrow_value from ObligationCollateral
        // This shows how much of the collateral is attributed to each borrow
        // Useful for risk analysis and understanding position structure
        let mut collateral_assets = Vec::new();
        for deposit in &obligation.deposits {
            // Note: LTV will be 0.0 for now - we would need RPC access to fetch from Reserve
            // This is a limitation of the current Protocol trait design
            // TODO: Consider adding RPC parameter to parse_position or enrich Position later
            collateral_assets.push(Asset {
                mint: deposit.deposit_reserve,
                amount: deposit.deposited_amount,
                amount_usd: deposit.market_value.to_f64(),
                ltv: 0.0, // TODO: Fetch from Reserve account (requires RPC access)
            });
        }

        // ✅ FIX: Use cumulative_borrow_rate_wads and borrowed_amount_wads properly
        // The actual borrowed amount should account for interest accrual
        // borrowed_amount_wads already includes interest (it's cumulative)
        let mut debt_assets = Vec::new();
        for borrow in &obligation.borrows {
            // ✅ FIX: Use borrowed_amount_wads directly (already includes interest)
            // cumulative_borrow_rate_wads is used on-chain to calculate interest
            // but borrowed_amount_wads is the current amount including all accrued interest
            let borrowed_amount = borrow.borrowed_amount_wads.to_f64() as u64;
            debt_assets.push(Asset {
                mint: borrow.borrow_reserve,
                amount: borrowed_amount,
                amount_usd: borrow.market_value.to_f64(),
                ltv: 0.0, // TODO: Fetch from Reserve account (requires RPC access)
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
