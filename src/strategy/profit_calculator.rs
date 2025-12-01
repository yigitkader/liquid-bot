use crate::core::types::Opportunity;
use crate::core::config::Config;

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
        let dex_fee_bps = self.config.dex_fee_bps as f64;
        let fee = size_usd * (dex_fee_bps / 10_000.0);
        log::debug!(
            "ProfitCalculator: dex_fee -> size_usd={:.6}, dex_fee_bps={}, fee={:.6}",
            size_usd,
            dex_fee_bps,
            fee
        );
        fee
    }
}
