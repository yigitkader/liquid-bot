use crate::core::types::Opportunity;
use crate::core::config::Config;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;
use once_cell::sync::Lazy;
use std::collections::HashSet;

pub struct ProfitCalculator {
    config: Config,
}

impl ProfitCalculator {
    pub fn new(config: Config) -> Self {
        ProfitCalculator { config }
    }

    /// Calculate net profit for an opportunity
    /// 
    /// # Arguments
    /// * `opportunity` - The liquidation opportunity
    /// * `hop_count` - Number of hops in the swap route (for multi-hop swaps)
    ///                 If None, defaults to 1 hop (single-hop swap assumption)
    pub fn calculate_net_profit(&self, opportunity: &Opportunity, hop_count: Option<u8>) -> f64 {
        let gross = opportunity.seizable_collateral as f64 / 1_000_000.0
            - opportunity.max_liquidatable as f64 / 1_000_000.0;

        let tx_fee = self.calculate_tx_fee();
        let slippage_cost = self.calculate_slippage_cost(opportunity);
        // ✅ Use hop_count for accurate multi-hop fee calculation
        let dex_fee = self.calculate_dex_fee(opportunity, hop_count);

        let net = gross - tx_fee - slippage_cost - dex_fee;

        log::debug!(
            "ProfitCalculator: position={} gross={:.6}, tx_fee={:.6}, slippage_cost={:.6}, dex_fee={:.6} (hop_count={:?}), net={:.6}",
            opportunity.position.address,
            gross,
            tx_fee,
            slippage_cost,
            dex_fee,
            hop_count,
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

    /// Calculate DEX fee for a swap
    /// 
    /// ✅ CRITICAL FIX: Multi-hop swaplar için fee hop sayısına göre hesaplanır
    /// Example: USDC -> SOL -> ETH (2 hops) için fee 2x olur
    /// 
    /// # Arguments
    /// * `opp` - Opportunity with debt and collateral mints
    /// * `hop_count` - Number of hops in the swap route (default: 1 if not provided)
    fn calculate_dex_fee(&self, opp: &Opportunity, hop_count: Option<u8>) -> f64 {
        let needs_swap = opp.debt_mint != opp.collateral_mint;
        
        if !needs_swap {
            log::debug!(
                "ProfitCalculator: dex_fee -> no swap needed (debt_mint == collateral_mint), fee=0"
            );
            return 0.0;
        }

        let size_usd = opp.seizable_collateral as f64 / 1_000_000.0;
        let hop_count = hop_count.unwrap_or(1); // Default to 1 hop if not provided
        
        // CRITICAL FIX: Detect stablecoin pairs and apply lower fee
        // Stablecoin pairs (USDC/USDT) typically have much lower DEX fees (~0.01% vs 0.2%)
        let is_stablecoin_pair = self.is_stablecoin_pair(&opp.debt_mint, &opp.collateral_mint);
        
        let base_dex_fee_bps = if is_stablecoin_pair {
            // Stablecoin pairs: Use much lower fee (0.01% = 1 bps)
            // This is typical for stablecoin swaps on most DEXes
            const STABLECOIN_DEX_FEE_BPS: u16 = 1; // 0.01%
            log::debug!(
                "ProfitCalculator: dex_fee -> stablecoin pair detected, using lower fee ({} bps per hop)",
                STABLECOIN_DEX_FEE_BPS
            );
            STABLECOIN_DEX_FEE_BPS as f64
        } else {
            // Regular pairs: Use configured fee
            self.config.dex_fee_bps as f64
        };
        
        // ✅ CRITICAL: Multiply fee by hop count
        // Each hop in a multi-hop swap incurs a fee
        // Example: USDC -> SOL -> ETH (2 hops) = base_fee * 2
        let total_dex_fee_bps = base_dex_fee_bps * hop_count as f64;
        let fee = size_usd * (total_dex_fee_bps / 10_000.0);
        
        log::debug!(
            "ProfitCalculator: dex_fee -> size_usd={:.6}, base_fee_bps={}, hop_count={}, total_fee_bps={}, fee={:.6}, stablecoin_pair={}",
            size_usd,
            base_dex_fee_bps,
            hop_count,
            total_dex_fee_bps,
            fee,
            is_stablecoin_pair
        );
        fee
    }
    
    /// Check if two mints form a stablecoin pair (e.g., USDC/USDT, DAI/USDC)
    /// Stablecoin pairs typically have much lower DEX fees (~0.01% vs 0.2%)
    /// 
    /// ✅ CRITICAL FIX: Use static HashSet for O(1) lookups instead of parsing on every call
    /// This prevents 16,000+ parse operations per second in high-throughput scenarios
    fn is_stablecoin_pair(&self, mint1: &Pubkey, mint2: &Pubkey) -> bool {
        STABLECOIN_SET.contains(mint1) && STABLECOIN_SET.contains(mint2)
    }
}

// ✅ CRITICAL FIX: Pre-computed HashSet of stablecoin Pubkeys for O(1) lookups
// This eliminates the need to parse strings on every opportunity check
// Performance improvement: 16,000 parse/sec → 16,000 HashSet.contains()/sec (much faster)
//
// ✅ CRITICAL FIX: Explicit error handling - don't silently ignore parse errors!
// Problem: filter_map() silently skips invalid mints → HashSet incomplete → wrong fee calculation
// Solution: Panic on parse errors to catch configuration mistakes early
static STABLECOIN_SET: Lazy<HashSet<Pubkey>> = Lazy::new(|| {
    let mints = vec![
        "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", // USDC
        "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB", // USDT
        "EjmyN6qEC1Tf1JxiG1ae7UTJhUxSwk1TCWNWqxWV4J6o", // DAI
        "FR87nWEUxVgerFGhZM8Y4AggKGLnaXswr1Pd8wZ4kZcp", // FRAX
        "9vMJfxuKxXBoEa7rM12mYLMwTacLMLDJqHozw96WQL8i", // UST (TerraUSD)
        "AZsHEMXd36Bj1EMNXhowJajpUXzrKcK57wW4ZGXVa7yR", // BUSD
        "4k3Dyjzvzp8eMZWUXbBCjEvwSkkk59S5iCNLY3QrkX6R", // TUSD
        "EchesyfXePKdLbiHRbgTbYq4qP8zF8LzF6S9X5YJ7KzN", // USDP (Pax Dollar)
    ];
    
    // ✅ CRITICAL FIX: Explicit error handling - don't silently ignore parse errors!
    // Problem: filter_map() silently skips invalid mints → HashSet incomplete → wrong fee calculation
    // Solution: Panic on parse errors to catch configuration mistakes early
    let mut set = HashSet::new();
    for s in mints {
        match Pubkey::from_str(s) {
            Ok(pk) => {
                set.insert(pk);
            }
            Err(e) => {
                // ✅ Log error - don't silently ignore!
                eprintln!("FATAL: Invalid stablecoin mint '{}': {}", s, e);
                panic!("Stablecoin configuration error: Failed to parse mint '{}': {}", s, e);
            }
        }
    }
    set
});
