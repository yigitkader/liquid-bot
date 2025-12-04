use crate::core::config::Config;
use crate::core::types::Opportunity;
use anyhow::Result;
use once_cell::sync::Lazy;
use serde::Deserialize;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashSet;
use std::str::FromStr;
use std::time::Duration;

pub struct ProfitCalculator {
    config: Config,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct SwapInfo {
    #[serde(rename = "ammKey")]
    #[allow(dead_code)] // Required for deserialization, may be used for route analysis
    amm_key: Option<String>,
    label: Option<String>,
    #[serde(rename = "inputMint")]
    #[allow(dead_code)] // Required for deserialization, may be used for route analysis
    input_mint: Option<String>,
    #[serde(rename = "outputMint")]
    #[allow(dead_code)] // Required for deserialization, may be used for route analysis
    output_mint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RoutePlan {
    #[serde(rename = "swapInfo")]
    swap_info: Option<SwapInfo>,
    #[allow(dead_code)] // Required for deserialization, may be used for route analysis
    percent: Option<u8>,
}

#[derive(Debug, Deserialize)]
struct JupiterRouteInfo {
    #[serde(rename = "routePlan")]
    route_plan: Option<Vec<RoutePlan>>,
}

impl ProfitCalculator {
    pub fn new(config: Config) -> Self {
        ProfitCalculator {
            config,
            client: reqwest::Client::new(),
        }
    }

    pub async fn calculate_net_profit(&self, opportunity: &Opportunity, hop_count: Option<u8>) -> f64 {
        let gross = opportunity.seizable_collateral as f64 / 1_000_000.0
            - opportunity.max_liquidatable as f64 / 1_000_000.0;

        let tx_fee = self.calculate_tx_fee();
        let slippage_cost = self.calculate_slippage_cost(opportunity);
        let dex_fee = self.calculate_dex_fee(opportunity, hop_count).await;

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
        
        // ✅ CRITICAL FIX: Use ceil() for conservative priority fee calculation
        // Problem: Previous code used integer division (priority_fee_micro_lamports / 1_000_000)
        //   Integer division always rounds down, causing systematic underestimation of priority fee
        //   Example: 1_500_000 micro-lamports / 1_000_000 = 1 lamport (should be 1.5)
        //   This bias can cause profitable trades to appear unprofitable at profit margins
        // Solution: Use ceil() to round up (conservative approach)
        //   - Ensures we don't underestimate transaction fees
        //   - Prevents losses from fee underestimation in marginal profit scenarios
        //   - Formula: ceil(micro_lamports / 1_000_000.0)
        // priority_fee_per_cu is in micro-lamports per CU (1 micro-lamport = 0.000001 lamport)
        // Formula: ceil((compute_units × priority_fee_per_cu) / 1_000_000.0)
        let priority_fee_micro_lamports = self.config.liquidation_compute_units as u64
            * self.config.priority_fee_per_cu;
        let priority_fee = (priority_fee_micro_lamports as f64 / 1_000_000.0).ceil() as u64;
        
        let total_lamports = base_fee + priority_fee;
        let total_usd = total_lamports as f64 * self.config.sol_price_fallback_usd / 1e9;
        log::debug!(
            "ProfitCalculator: tx_fee -> base_fee_lamports={}, priority_fee_micro_lamports={}, priority_fee_lamports={} (ceil), total_lamports={}, sol_price_fallback_usd={}, total_usd={:.6}",
            base_fee,
            priority_fee_micro_lamports,
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

    async fn calculate_dex_fee(&self, opp: &Opportunity, hop_count: Option<u8>) -> f64 {
        let needs_swap = opp.debt_mint != opp.collateral_mint;

        if !needs_swap {
            log::debug!(
                "ProfitCalculator: dex_fee -> no swap needed (debt_mint == collateral_mint), fee=0"
            );
            return 0.0;
        }

        let size_usd = opp.seizable_collateral as f64 / 1_000_000.0;
        
        // ✅ FIX: Use conservative estimate when hop_count is None (Jupiter API failed)
        // Problem: Previous code used 2 hops for non-stablecoin pairs, but 3-hop swaps exist (e.g., ETH → SOL → USDC)
        //   This underestimates DEX fees (2 hop × 0.2% = 0.4% vs actual 3 hop × 0.2% = 0.6%)
        //   Example: $10,000 swap with 3 hops but estimated as 2 hops = $20 fee underestimation
        // Solution: Use conservative estimate to avoid fee underestimation
        //   - Stablecoin pairs: 1 hop (direct swap, very reliable)
        //   - Regular pairs: 3 hops (conservative - covers most cases including 3-hop swaps)
        //   Note: This may slightly overestimate fees for 1-2 hop swaps, but prevents losses from 3-hop swaps
        let hop_count = if let Some(count) = hop_count {
            count
        } else {
            // ✅ FIX: Improved hop count estimation for Jupiter API failures
            // Problem: Previous code used 3 hops for all non-stablecoin pairs, but major pairs
            //   (ETH-SOL, BTC-SOL, etc.) typically use 2 hops, not 3
            //   This overestimates fees (3 hop × 20 bps = 60 bps vs actual 2 hop × 20 bps = 40 bps)
            //   Example: $10,000 swap with 2 hops but estimated as 3 hops = $20 fee overestimation
            //   Result: Profitable opportunities are rejected due to overestimated fees
            // Solution: Check if pair is major pair (SOL, ETH, BTC, USDC, USDT combinations)
            //   - Stablecoin pairs: 1 hop (direct swap, very reliable)
            //   - Major pairs: 2 hops (SOL-ETH, SOL-BTC, ETH-USDC, etc. - typically 2 hops)
            //   - Other pairs: 3 hops (conservative default for less liquid pairs)
            let is_stablecoin_pair = self.is_stablecoin_pair(&opp.debt_mint, &opp.collateral_mint);
            let estimated_hop_count = if is_stablecoin_pair {
                1 // Stablecoin pairs usually direct swap (USDC ↔ USDT) - very reliable
            } else if self.is_major_pair(&opp.debt_mint, &opp.collateral_mint) {
                2 // Major pairs (SOL-ETH, SOL-BTC, ETH-USDC, etc.) typically 2 hops
                  // Examples: ETH → SOL → USDC (2 hops), BTC → SOL → USDC (2 hops)
                  // These pairs have high liquidity and direct routes, usually 2 hops max
            } else {
                3 // Other pairs: Use 3 hops as conservative default to cover multi-hop swaps
                  // Examples: Small cap token → SOL → USDC → USDT (3 hops)
                  // This prevents fee underestimation for less liquid pairs
            };
            log::warn!(
                "ProfitCalculator: hop_count is None for {} -> {}, using estimate: {} hops (stablecoin_pair: {}, major_pair: {})",
                opp.debt_mint,
                opp.collateral_mint,
                estimated_hop_count,
                is_stablecoin_pair,
                self.is_major_pair(&opp.debt_mint, &opp.collateral_mint)
            );
            estimated_hop_count
        };

        // ✅ CRITICAL FIX: Use Jupiter API for all pairs, not just stablecoin pairs
        // Problem: 
        //   1. Stablecoin pairs: Jupiter API fail ederse 5 bps fallback kullanılıyor
        //      Gerçek fee: Orca direct pool 1 bps × 1 hop = 1 bps
        //      4 bps overestimation → profit 4 bps azalır ($10k swap'te $4 loss)
        //   2. Regular pairs: Jupiter API hiç kullanılmıyor, sadece config.dex_fee_bps
        //      Bu yanlış fee estimate'e yol açabilir
        // Solution:
        //   1. Tüm pair'ler için Jupiter API kullan (stablecoin + regular)
        //   2. Fallback fee'leri düzelt:
        //      - Stablecoin fallback: 1 bps (Orca direct pool, en yaygın)
        //      - Regular fallback: config.dex_fee_bps (mevcut config değeri)
        let is_stablecoin_pair = self.is_stablecoin_pair(&opp.debt_mint, &opp.collateral_mint);

        // ✅ Try Jupiter API for all pairs (stablecoin and regular)
        let base_dex_fee_bps = if let Ok(route_info) = self.get_jupiter_route_info(opp.debt_mint, opp.collateral_mint).await {
            // Got actual fee from Jupiter API route
            let actual_fee_bps = self.estimate_fee_from_route(&route_info);
            log::debug!(
                "ProfitCalculator: dex_fee -> got actual fee from Jupiter API: {} bps per hop (stablecoin_pair: {})",
                actual_fee_bps,
                is_stablecoin_pair
            );
            actual_fee_bps as f64
        } else {
            // Jupiter API failed - use appropriate fallback
            if is_stablecoin_pair {
                // ✅ FIX: Stablecoin fallback should be 1 bps (Orca direct pool), not 5 bps
                // Orca USDC/USDT direct pool: 1 bps (0.01%) - most common for stablecoin swaps
                // Previous 5 bps was too high, causing 4 bps overestimation
                const STABLECOIN_FEE_FALLBACK: u16 = 1;
                log::warn!(
                    "ProfitCalculator: dex_fee -> stablecoin pair detected, Jupiter API failed, using fallback: {} bps per hop (Orca direct pool fee)",
                    STABLECOIN_FEE_FALLBACK
                );
                STABLECOIN_FEE_FALLBACK as f64
            } else {
                // Regular pairs: Use configured fee as fallback
                log::warn!(
                    "ProfitCalculator: dex_fee -> regular pair, Jupiter API failed, using config fallback: {} bps per hop",
                    self.config.dex_fee_bps
                );
                self.config.dex_fee_bps as f64
            }
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

    /// Get Jupiter route information for fee estimation
    async fn get_jupiter_route_info(&self, input_mint: Pubkey, output_mint: Pubkey) -> Result<JupiterRouteInfo> {
        use anyhow::Context;
        use tokio::time::timeout;

        if !self.config.use_jupiter_api {
            return Err(anyhow::anyhow!("Jupiter API is disabled"));
        }

        // Use a reasonable amount for quote (1M = 1 token with 6 decimals)
        let amount = 1_000_000u64;
        let url = format!(
            "https://quote-api.jup.ag/v6/quote?inputMint={}&outputMint={}&amount={}&slippageBps=50",
            input_mint, output_mint, amount
        );

        const JUPITER_TIMEOUT: Duration = Duration::from_secs(5);

        let response = timeout(
            JUPITER_TIMEOUT,
            self.client.get(&url).send()
        )
        .await
        .map_err(|e| {
            log::debug!("Jupiter API request timeout for fee info: {}", e);
            anyhow::anyhow!("Jupiter API timeout")
        })?
        .map_err(|e| {
            log::debug!("Jupiter API network error for fee info: {}", e);
            anyhow::anyhow!("Network error: {}", e)
        })?;

        if !response.status().is_success() {
            let status = response.status();
            log::debug!("Jupiter API HTTP error for fee info: {} {}", status.as_u16(), status.canonical_reason().unwrap_or("Unknown"));
            return Err(anyhow::anyhow!("HTTP {} {}", status.as_u16(), status));
        }

        let response_text = response
            .text()
            .await
            .context("Failed to read response body")?;

        let route_info: JupiterRouteInfo = serde_json::from_str(&response_text)
            .map_err(|e| {
                log::debug!("Jupiter API JSON parse error for fee info: {}", e);
                anyhow::anyhow!("Failed to parse JSON: {}", e)
            })?;

        Ok(route_info)
    }

    /// Estimate fee in bps from Jupiter route information
    /// Based on DEX labels in the route plan
    fn estimate_fee_from_route(&self, route_info: &JupiterRouteInfo) -> u16 {
        // Default fee if we can't determine from route
        const DEFAULT_STABLECOIN_FEE_BPS: u16 = 5;

        let route_plan = match &route_info.route_plan {
            Some(plan) if !plan.is_empty() => plan,
            _ => {
                log::debug!("ProfitCalculator: No route plan in Jupiter response, using default fee");
                return DEFAULT_STABLECOIN_FEE_BPS;
            }
        };

        // Check each hop's DEX to determine fee
        // Known stablecoin pool fees:
        // - Orca USDC/USDT: 1 bps (0.01%)
        // - Raydium stable pool: 4 bps (0.04%)
        // - Jupiter multi-hop: 20+ bps (0.2%+)
        let mut max_fee_bps = 1u16; // Start with minimum (Orca direct pool)

        for hop in route_plan {
            if let Some(swap_info) = &hop.swap_info {
                if let Some(label) = &swap_info.label {
                    let label_lower = label.to_lowercase();
                    // Check for known DEX labels and their typical fees
                    let hop_fee_bps = if label_lower.contains("orca") {
                        1 // Orca stablecoin pools: 1 bps
                    } else if label_lower.contains("raydium") {
                        4 // Raydium stable pools: 4 bps
                    } else if label_lower.contains("jupiter") || label_lower.contains("route") {
                        20 // Jupiter routing/multi-hop: 20+ bps
                    } else {
                        // Unknown DEX, use conservative estimate
                        5
                    };
                    max_fee_bps = max_fee_bps.max(hop_fee_bps);
                    log::debug!(
                        "ProfitCalculator: Route hop DEX '{}' estimated fee: {} bps",
                        label,
                        hop_fee_bps
                    );
                }
            }
        }

        log::debug!(
            "ProfitCalculator: Estimated fee from route: {} bps (from {} hop(s))",
            max_fee_bps,
            route_plan.len()
        );

        max_fee_bps
    }

    pub fn is_stablecoin_pair(&self, mint1: &Pubkey, mint2: &Pubkey) -> bool {
        // ✅ FIX: Handle initialization error gracefully
        // If sets failed to initialize (should never happen if init_stablecoin_sets() succeeded),
        // return false (conservative - treat as non-stablecoin pair)
        STABLECOIN_PAIRS
            .as_ref()
            .map(|pairs| pairs.contains(&(*mint1, *mint2)))
            .unwrap_or_else(|e| {
                log::error!("Stablecoin pairs not initialized: {} - treating as non-stablecoin pair", e);
                false
            })
    }

    /// Check if a pair is a major pair (SOL, ETH, BTC, USDC, USDT combinations)
    /// Major pairs typically have high liquidity and direct routes, usually 2 hops max
    /// Examples: ETH-SOL, BTC-SOL, ETH-USDC, BTC-USDC, etc.
    pub fn is_major_pair(&self, mint1: &Pubkey, mint2: &Pubkey) -> bool {
        // Get major token mints from config (with fallback to defaults)
        let sol_mint = self.config.sol_mint.parse::<Pubkey>().ok();
        let usdc_mint = self.config.usdc_mint.parse::<Pubkey>().ok();
        let usdt_mint = self.config.usdt_mint.as_ref()
            .and_then(|s| s.parse::<Pubkey>().ok());
        let eth_mint = self.config.eth_mint.as_ref()
            .and_then(|s| s.parse::<Pubkey>().ok());
        let btc_mint = self.config.btc_mint.as_ref()
            .and_then(|s| s.parse::<Pubkey>().ok());

        // Build set of major tokens
        let mut major_tokens = HashSet::new();
        if let Some(sol) = sol_mint {
            major_tokens.insert(sol);
        }
        if let Some(usdc) = usdc_mint {
            major_tokens.insert(usdc);
        }
        if let Some(usdt) = usdt_mint {
            major_tokens.insert(usdt);
        }
        if let Some(eth) = eth_mint {
            major_tokens.insert(eth);
        }
        if let Some(btc) = btc_mint {
            major_tokens.insert(btc);
        }

        // Check if both mints are major tokens
        // Major pairs: SOL-ETH, SOL-BTC, ETH-USDC, BTC-USDC, etc.
        // These pairs have high liquidity and typically use 2 hops
        major_tokens.contains(mint1) && major_tokens.contains(mint2)
    }
}

// ✅ FIX: Use Lazy<Result<...>> to allow initialization errors without panic
// Problem: Previous code used panic! when stablecoin mint parsing failed
//   This causes entire bot to crash in production, even if error occurs during runtime
// Solution: Use Result-based initialization that can be checked at startup
//   - init_stablecoin_sets() function validates and initializes sets
//   - main.rs calls this at startup and exits gracefully on error
//   - Runtime access uses unwrap_or_else with empty set fallback (should never happen if init succeeded)
static STABLECOIN_SET: Lazy<Result<HashSet<Pubkey>, String>> = Lazy::new(|| {
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

    let mut set = HashSet::new();
    for s in mints {
        match Pubkey::from_str(s) {
            Ok(pk) => {
                set.insert(pk);
            }
            Err(e) => {
                // ✅ FIX: Return error instead of panic
                // This allows main.rs to handle error gracefully at startup
                let error_msg = format!(
                    "Stablecoin configuration error: Failed to parse mint '{}': {}",
                    s, e
                );
                log::error!("{}", error_msg);
                return Err(error_msg);
            }
        }
    }
    Ok(set)
});

static STABLECOIN_PAIRS: Lazy<Result<HashSet<(Pubkey, Pubkey)>, String>> = Lazy::new(|| {
    // ✅ FIX: Handle STABLECOIN_SET initialization error
    let stablecoins = match STABLECOIN_SET.as_ref() {
        Ok(set) => set.iter().copied().collect::<Vec<Pubkey>>(),
        Err(e) => {
            let error_msg = format!("Failed to initialize stablecoin pairs: {}", e);
            log::error!("{}", error_msg);
            return Err(error_msg);
        }
    };
    
    let mut pairs = HashSet::new();
    for i in &stablecoins {
        for j in &stablecoins {
            if i != j {
                // Add both directions: (i, j) and (j, i)
                // This ensures O(1) lookup regardless of argument order
                pairs.insert((*i, *j));
                pairs.insert((*j, *i));
            }
        }
    }
    Ok(pairs)
});

/// Initialize stablecoin sets and validate configuration
/// 
/// This function should be called at startup (in main.rs) to ensure
/// stablecoin configuration is valid before the bot starts running.
/// 
/// Returns:
/// - Ok(()): Stablecoin sets initialized successfully
/// - Err(String): Configuration error (invalid mint address, etc.)
/// 
/// ✅ FIX: Fail-fast at startup instead of panic during runtime
/// Problem: Previous code used panic! when stablecoin mint parsing failed
///   This causes entire bot to crash, even if error occurs during runtime
/// Solution: Validate configuration at startup and exit gracefully on error
///   - This function checks if sets initialized successfully
///   - main.rs calls this and exits with error code if validation fails
///   - Runtime access uses unwrap_or_else (should never fail if init succeeded)
pub fn init_stablecoin_sets() -> Result<(), String> {
    // Force initialization by accessing the Lazy values
    // This will trigger parsing and return any errors
    STABLECOIN_SET.as_ref().map_err(|e| e.clone())?;
    STABLECOIN_PAIRS.as_ref().map_err(|e| e.clone())?;
    
    log::info!("✅ Stablecoin sets initialized successfully ({} stablecoins, {} pairs)", 
        STABLECOIN_SET.as_ref().unwrap().len(),
        STABLECOIN_PAIRS.as_ref().unwrap().len()
    );
    
    Ok(())
}
