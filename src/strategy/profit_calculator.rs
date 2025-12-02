use crate::core::types::Opportunity;
use crate::core::config::Config;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

pub struct ProfitCalculator {
    config: Config,
}

impl ProfitCalculator {
    pub fn new(config: Config) -> Self {
        ProfitCalculator { config }
    }

    pub fn calculate_net_profit(&self, opportunity: &Opportunity) -> f64 {
        let gross = opportunity.seizable_collateral as f64 / 1_000_000.0
            - opportunity.max_liquidatable as f64 / 1_000_000.0;

        let tx_fee = self.calculate_tx_fee();
        let slippage_cost = self.calculate_slippage_cost(opportunity);
        let dex_fee = self.calculate_dex_fee(opportunity);

        let net = gross - tx_fee - slippage_cost - dex_fee;

        log::debug!(
            "ProfitCalculator: position={} gross={:.6}, tx_fee={:.6}, slippage_cost={:.6}, dex_fee={:.6}, net={:.6}",
            opportunity.position.address,
            gross,
            tx_fee,
            slippage_cost,
            dex_fee,
            net
        );

        net
    }

    fn calculate_tx_fee(&self) -> f64 {
        let base_fee = self.config.base_transaction_fee_lamports;
        let priority_fee = self.config.liquidation_compute_units as u64
            * self.config.priority_fee_per_cu / 1_000_000;
        let total_lamports = base_fee + priority_fee;
        let total_usd = total_lamports as f64 * self.config.sol_price_fallback_usd / 1e9;
        log::debug!(
            "ProfitCalculator: tx_fee -> base_fee_lamports={}, priority_fee_lamports={}, total_lamports={}, sol_price_fallback_usd={}, total_usd={:.6}",
            base_fee,
            priority_fee,
            total_lamports,
            self.config.sol_price_fallback_usd,
            total_usd
        );
        total_usd
    }

    fn calculate_slippage_cost(&self, opp: &Opportunity) -> f64 {
        let size_usd = opp.seizable_collateral as f64 / 1_000_000.0;
        let slippage_bps = self.config.max_slippage_bps as f64;
        let cost = size_usd * (slippage_bps / 10_000.0);
        log::debug!(
            "ProfitCalculator: slippage_cost -> size_usd={:.6}, slippage_bps={}, cost={:.6}",
            size_usd,
            slippage_bps,
            cost
        );
        cost
    }

    fn calculate_dex_fee(&self, opp: &Opportunity) -> f64 {
        let needs_swap = opp.debt_mint != opp.collateral_mint;
        
        if !needs_swap {
            log::debug!(
                "ProfitCalculator: dex_fee -> no swap needed (debt_mint == collateral_mint), fee=0"
            );
            return 0.0;
        }

        let size_usd = opp.seizable_collateral as f64 / 1_000_000.0;
        
        // CRITICAL FIX: Detect stablecoin pairs and apply lower fee
        // Stablecoin pairs (USDC/USDT) typically have much lower DEX fees (~0.01% vs 0.2%)
        let is_stablecoin_pair = self.is_stablecoin_pair(&opp.debt_mint, &opp.collateral_mint);
        
        let dex_fee_bps = if is_stablecoin_pair {
            // Stablecoin pairs: Use much lower fee (0.01% = 1 bps)
            // This is typical for stablecoin swaps on most DEXes
            const STABLECOIN_DEX_FEE_BPS: u16 = 1; // 0.01%
            log::debug!(
                "ProfitCalculator: dex_fee -> stablecoin pair detected, using lower fee ({} bps)",
                STABLECOIN_DEX_FEE_BPS
            );
            STABLECOIN_DEX_FEE_BPS as f64
        } else {
            // Regular pairs: Use configured fee
            self.config.dex_fee_bps as f64
        };
        
        let fee = size_usd * (dex_fee_bps / 10_000.0);
        log::debug!(
            "ProfitCalculator: dex_fee -> size_usd={:.6}, dex_fee_bps={}, fee={:.6}, stablecoin_pair={}",
            size_usd,
            dex_fee_bps,
            fee,
            is_stablecoin_pair
        );
        fee
    }
    
    /// Check if two mints form a stablecoin pair (e.g., USDC/USDT)
    /// Stablecoin pairs typically have much lower DEX fees (~0.01% vs 0.2%)
    fn is_stablecoin_pair(&self, mint1: &Pubkey, mint2: &Pubkey) -> bool {
        // Get stablecoin mint addresses from config
        let usdc_mint = Pubkey::from_str(&self.config.usdc_mint).ok();
        let usdt_mint = self.config.usdt_mint.as_ref()
            .and_then(|s| Pubkey::from_str(s).ok());
        
        // Check if both mints are stablecoins
        let mint1_is_stablecoin = usdc_mint.map(|m| m == *mint1).unwrap_or(false)
            || usdt_mint.map(|m| m == *mint1).unwrap_or(false);
        
        let mint2_is_stablecoin = usdc_mint.map(|m| m == *mint2).unwrap_or(false)
            || usdt_mint.map(|m| m == *mint2).unwrap_or(false);
        
        mint1_is_stablecoin && mint2_is_stablecoin
    }
}
