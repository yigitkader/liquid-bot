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

        // FAST PATH 1: Size check (1200-1500 bytes expected for obligation accounts)
        // This quickly filters out wrong account types before expensive parsing
        const MIN_OBLIGATION_SIZE: usize = 1200;
        const MAX_OBLIGATION_SIZE: usize = 1500;
        
        if data_len < MIN_OBLIGATION_SIZE || data_len > MAX_OBLIGATION_SIZE {
            return None; // Wrong account type, skip immediately
        }
        
        // FAST PATH 2: Discriminator check (before expensive parse)
        // This filters out wrong account types by checking the account discriminator
        // Solend uses Anchor framework which prefixes accounts with discriminator
        if data_len >= 8 {
            let account_discriminator = &account.data[0..8];
            let expected_discriminator = get_obligation_discriminator();
            
            if account_discriminator != expected_discriminator {
                return None; // Wrong account type, skip immediately
            }
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
        
        // FAST PATH 3: Skip empty obligations immediately
        if obligation.deposits.is_empty() && obligation.borrows.is_empty() {
            return None;
        }
        
        // FAST PATH 4: Quick health factor check BEFORE expensive USD calculations
        // This filters out healthy positions early, avoiding unnecessary work
        // ✅ CRITICAL FIX: Use the SAME threshold calculation as analyzer for consistency
        // Analyzer uses: HF < (threshold * safety_margin) to liquidate
        // Parser should use: HF > (threshold * safety_margin) to skip (inverse logic)
        // This ensures parser and analyzer agree on which positions are liquidatable
        let health_factor = obligation.calculate_health_factor();
        
        // ✅ CRITICAL FIX: Use threshold * safety_margin (NOT threshold / safety_margin)
        // This matches analyzer's logic: analyzer liquidates when HF < threshold * safety_margin
        // So parser should skip when HF > threshold * safety_margin (strict inequality to match analyzer)
        // Example: threshold=1.0, safety_margin=0.95
        //   - Analyzer: liquidate if HF < 0.95 (strict inequality)
        //   - Parser: skip if HF > 0.95 (strict inequality for symmetry)
        // ✅ FIX: Use > (strict inequality) instead of >= to match analyzer's strict inequality (<)
        // This ensures consistent handling of boundary cases:
        //   - HF = 0.95 exactly: Parser parses, Analyzer doesn't liquidate (both use strict inequality)
        //   - HF = 0.949: Parser parses, Analyzer liquidates (both use strict inequality)
        //   - HF = 0.951: Parser skips, Analyzer doesn't liquidate (both use strict inequality)
        // Using strict inequality on both sides ensures symmetry and prevents edge case inconsistencies
        let skip_threshold = self.config.hf_liquidation_threshold * self.config.liquidation_safety_margin;
        if health_factor > skip_threshold {
            return None;
        }
        
        // Only now do expensive USD calculations for potentially liquidatable positions
        let collateral_usd = obligation.total_deposited_value_usd();
        let debt_usd = obligation.total_borrowed_value_usd();
        
        // Zero-value obligation check (after health factor, but still useful)
        if collateral_usd < 0.01 && debt_usd < 0.01 {
            return None;
        }

        // Log detaylı info (sadece ilk birkaç kez) - debug level to avoid production log spam
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

/// Get the obligation account discriminator
/// Solend uses Anchor framework which prefixes accounts with discriminator
/// Discriminator is the first 8 bytes of SHA256("account:Obligation")
fn get_obligation_discriminator() -> [u8; 8] {
    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    hasher.update(b"account:Obligation");
    let hash = hasher.finalize();
    let mut discriminator = [0u8; 8];
    discriminator.copy_from_slice(&hash[..8]);
    discriminator
}
