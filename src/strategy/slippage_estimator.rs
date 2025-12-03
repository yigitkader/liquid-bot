use crate::core::config::Config;
use anyhow::Result;
use serde::Deserialize;
use solana_sdk::pubkey::Pubkey;
use std::time::Duration;

pub struct SlippageEstimator {
    config: Config,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct SwapInfo {
    #[serde(rename = "ammKey")]
    amm_key: Option<String>,
    label: Option<String>,
    #[serde(rename = "inputMint")]
    input_mint: Option<String>,
    #[serde(rename = "outputMint")]
    output_mint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RoutePlan {
    #[serde(rename = "swapInfo")]
    swap_info: Option<SwapInfo>,
    percent: Option<u8>,
}

#[derive(Debug, Deserialize)]
struct JupiterQuoteResponse {
    /// Jupiter API v6 returns priceImpactPct as a STRING in percentage format (0-100)
    /// Examples: "0" = 0%, "0.5" = 0.5%, "1.25" = 1.25%
    /// This must be parsed to f64 and converted to basis points (multiply by 100)
    #[serde(rename = "priceImpactPct")]
    price_impact_pct: Option<String>,
    /// Route plan contains the swap path information
    /// Each element in routePlan represents one hop in the swap
    /// Example: USDC -> SOL -> ETH would have 2 hops (routePlan.length = 2)
    #[serde(rename = "routePlan")]
    route_plan: Option<Vec<RoutePlan>>,
}

impl SlippageEstimator {
    pub fn new(config: Config) -> Self {
        SlippageEstimator {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// Estimate DEX slippage and return both slippage (in bps) and hop count
    /// Hop count is important for multi-hop swaps (e.g., USDC -> SOL -> ETH = 2 hops)
    /// Each hop incurs a DEX fee, so total fee = base_fee * hop_count
    pub async fn estimate_dex_slippage_with_route(
        &self,
        input_mint: Pubkey,
        output_mint: Pubkey,
        amount: u64,
    ) -> Result<(u16, u8)> {
        if self.config.use_jupiter_api {
            self.estimate_with_jupiter_route(input_mint, output_mint, amount)
                .await
        } else {
            // Fallback: assume 1 hop (direct swap)
            let slippage = self.estimate_with_multipliers(amount)?;
            Ok((slippage, 1))
        }
    }

    pub async fn estimate_dex_slippage(
        &self,
        input_mint: Pubkey,
        output_mint: Pubkey,
        amount: u64,
    ) -> Result<u16> {
        if self.config.use_jupiter_api {
            let (slippage, _) = self
                .estimate_with_jupiter_route(input_mint, output_mint, amount)
                .await?;
            Ok(slippage)
        } else {
            self.estimate_with_multipliers(amount)
        }
    }

    /// Estimate slippage with Jupiter API and return both slippage and hop count
    /// ✅ FIX: Refactored to use cleaner error handling pattern
    async fn estimate_with_jupiter_route(
        &self,
        input_mint: Pubkey,
        output_mint: Pubkey,
        amount: u64,
    ) -> Result<(u16, u8)> {
        const MAX_RETRIES: u32 = 3;
        let url = format!(
            "https://quote-api.jup.ag/v6/quote?inputMint={}&outputMint={}&amount={}&slippageBps=50",
            input_mint, output_mint, amount
        );

        // Helper function for exponential backoff
        // Exponential backoff: 1s, 2s, 4s (better for rate limit handling)
        let get_backoff = |attempt: u32| -> Duration {
            // attempt 1: 1000 * 2^0 = 1000ms = 1s
            // attempt 2: 1000 * 2^1 = 2000ms = 2s
            // attempt 3: 1000 * 2^2 = 4000ms = 4s
            let backoff_ms = 1000 * 2_u64.pow(attempt.saturating_sub(1));
            Duration::from_millis(backoff_ms)
        };

        let mut last_error = None;

        // ✅ FIX: Cleaner retry loop - try Jupiter API, fallback on all errors
        for attempt in 1..=MAX_RETRIES {
            match self.try_jupiter_quote(&url).await {
                Ok((slippage, hop_count)) => {
                    // ✅ Success - return immediately
                    return Ok((slippage, hop_count));
                }
                Err(e) => {
                    log::warn!(
                        "Jupiter API attempt {}/{} failed: {}",
                        attempt,
                        MAX_RETRIES,
                        e
                    );
                    last_error = Some(e);

                    if attempt < MAX_RETRIES {
                        let backoff = get_backoff(attempt);
                        tokio::time::sleep(backoff).await;
                        // continue to next attempt
                    }
                }
            }
        }

        // ✅ All retries exhausted - use fallback
        log::warn!(
            "Jupiter API failed after {} attempts (last error: {:?}), using fallback",
            MAX_RETRIES,
            last_error
        );

        let slippage = self.estimate_with_multipliers(amount)?;
        Ok((slippage, 1))
    }

    /// Try to get a quote from Jupiter API (single attempt, no retries)
    /// ✅ FIX: Separated into its own function for cleaner error handling
    async fn try_jupiter_quote(&self, url: &str) -> Result<(u16, u8)> {
        use anyhow::Context;
        use tokio::time::{timeout, Duration};

        // ✅ FIX: Add timeout to prevent infinite hang if Jupiter API hangs
        const JUPITER_TIMEOUT: Duration = Duration::from_secs(5);

        // Make HTTP request with timeout
        let response = timeout(
            JUPITER_TIMEOUT,
            self.client.get(url).send()
        )
        .await
        .map_err(|_| anyhow::anyhow!("Jupiter API timeout after {:?}", JUPITER_TIMEOUT))?
        .context("Network error")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!("HTTP error: {}", response.status()));
        }

        // Read response body
        let response_text = response
            .text()
            .await
            .context("Failed to read response body")?;

        // Parse JSON response
        let quote: JupiterQuoteResponse =
            serde_json::from_str(&response_text).context("Failed to parse JSON")?;

        // Extract price impact
        let price_impact_str = quote
            .price_impact_pct
            .ok_or_else(|| anyhow::anyhow!("No price impact in response"))?;

        // Parse price impact percentage
        let price_impact_pct = price_impact_str
            .parse::<f64>()
            .context("Failed to parse price impact")?;

        // ✅ Validate BEFORE using
        if price_impact_pct < 0.0 {
            return Err(anyhow::anyhow!(
                "Negative price impact: {}",
                price_impact_pct
            ));
        }

        if price_impact_pct > 100.0 {
            return Err(anyhow::anyhow!(
                "Suspiciously high price impact: {}%",
                price_impact_pct
            ));
        }

        // Convert percentage to basis points
        let slippage_bps = (price_impact_pct * 100.0) as u16;

        // Calculate hop count from route plan and log route details
        let hop_count = quote
            .route_plan
            .as_ref()
            .map(|route| route.len() as u8)
            .unwrap_or(1);

        // Log route plan details for debugging and monitoring
        if let Some(ref route_plan) = quote.route_plan {
            log::debug!(
                "Jupiter route plan: {} hop(s), details: {:?}",
                hop_count,
                route_plan
                    .iter()
                    .enumerate()
                    .map(|(i, hop)| {
                        format!(
                            "Hop {}: AMM={:?}, Label={:?}, Input={:?}, Output={:?}, Percent={:?}",
                            i + 1,
                            hop.swap_info.as_ref().and_then(|s| s.amm_key.as_ref()),
                            hop.swap_info.as_ref().and_then(|s| s.label.as_ref()),
                            hop.swap_info.as_ref().and_then(|s| s.input_mint.as_ref()),
                            hop.swap_info.as_ref().and_then(|s| s.output_mint.as_ref()),
                            hop.percent
                        )
                    })
                    .collect::<Vec<_>>()
            );
        }

        Ok((slippage_bps.min(self.config.max_slippage_bps), hop_count))
    }

    fn estimate_with_multipliers(&self, amount: u64) -> Result<u16> {
        let size_usd = amount as f64 / 1_000_000.0;
        let multiplier = if size_usd < self.config.slippage_size_small_threshold_usd {
            self.config.slippage_multiplier_small
        } else if size_usd > self.config.slippage_size_large_threshold_usd {
            self.config.slippage_multiplier_large
        } else {
            self.config.slippage_multiplier_medium
        };

        Ok((self.config.max_slippage_bps as f64 * multiplier) as u16)
    }

    pub async fn read_oracle_confidence(&self, mint: Pubkey) -> Result<u16> {
        use crate::blockchain::rpc_client::RpcClient;
        use crate::protocol::oracle::{get_pyth_oracle_account, read_pyth_price};
        use anyhow::Context;
        use std::sync::Arc;

        let rpc = Arc::new(
            RpcClient::new(self.config.rpc_http_url.clone())
                .context("Failed to create RPC client for oracle confidence")?,
        );

        if let Some(oracle_account) = get_pyth_oracle_account(&mint, Some(&self.config))? {
            if let Some(price_data) =
                read_pyth_price(&oracle_account, Arc::clone(&rpc), Some(&self.config)).await?
            {
                let confidence_pct = (price_data.confidence / price_data.price) * 100.0;
                let confidence_bps = (confidence_pct * 100.0) as u16;
                Ok(confidence_bps.min(10000))
            } else {
                Ok(self.config.default_oracle_confidence_slippage_bps)
            }
        } else {
            Ok(self.config.default_oracle_confidence_slippage_bps)
        }
    }
}
