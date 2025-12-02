use crate::core::config::Config;
use solana_sdk::pubkey::Pubkey;
use anyhow::{Context, Result};
use serde::Deserialize;
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
            self.estimate_with_jupiter_route(input_mint, output_mint, amount).await
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
            let (slippage, _) = self.estimate_with_jupiter_route(input_mint, output_mint, amount).await?;
            Ok(slippage)
        } else {
            self.estimate_with_multipliers(amount)
        }
    }

    /// Estimate slippage with Jupiter API and return both slippage and hop count
    async fn estimate_with_jupiter_route(
        &self,
        input_mint: Pubkey,
        output_mint: Pubkey,
        amount: u64,
    ) -> Result<(u16, u8)> {
        const MAX_RETRIES: u32 = 3;
        let url = format!(
            "https://quote-api.jup.ag/v6/quote?inputMint={}&outputMint={}&amount={}&slippageBps=50",
            input_mint,
            output_mint,
            amount
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

        // Retry loop with exponential backoff
        for attempt in 1..=MAX_RETRIES {
            match self.client.get(&url).send().await {
                Ok(response) => {
                    let status = response.status();
                    
                    if status.is_success() {
                        // Success - parse and return
                        let response_text = match response.text().await {
                            Ok(text) => text,
                            Err(e) => {
                                log::warn!(
                                    "Jupiter API: failed to read response body (attempt {}): {}",
                                    attempt,
                                    e
                                );
                                if attempt < MAX_RETRIES {
                                    let backoff = get_backoff(attempt);
                                    log::debug!("Jupiter API: waiting {:?} before retry (attempt {})", backoff, attempt);
                                    tokio::time::sleep(backoff).await;
                                    continue;
                                } else {
                                    log::warn!(
                                        "Jupiter API failed after {} attempts, using fallback",
                                        MAX_RETRIES
                                    );
                                    let slippage = self.estimate_with_multipliers(amount)?;
                                    return Ok((slippage, 1)); // Default to 1 hop on fallback
                                }
                            }
                        };

                        match serde_json::from_str::<JupiterQuoteResponse>(&response_text) {
                            Ok(quote) => {
                                if let Some(price_impact_str) = quote.price_impact_pct {
                                    // ✅ CRITICAL: Jupiter API v6 returns priceImpactPct as STRING in percentage format
                                    // Official docs: https://docs.jup.ag/apis/quote-api
                                    // Format: String representing percentage (0-100)
                                    // Examples: "0" = 0%, "0.5" = 0.5% = 50 bps, "1.25" = 1.25% = 125 bps
                                    // Note: "0" is normal for very liquid pairs (SOL/USDC)
                                    
                                    // Parse string to f64
                                    let price_impact_pct = match price_impact_str.parse::<f64>() {
                                        Ok(val) => {
                                            if val < 0.0 {
                                                log::warn!(
                                                    "⚠️  Jupiter API returned negative price impact: '{}' - using fallback",
                                                    price_impact_str
                                                );
                                                if attempt < MAX_RETRIES {
                                                    let backoff = get_backoff(attempt);
                                                    log::debug!("Jupiter API: waiting {:?} before retry (attempt {})", backoff, attempt);
                                                    tokio::time::sleep(backoff).await;
                                                    continue;
                                                } else {
                                                    let slippage = self.estimate_with_multipliers(amount)?;
                                    return Ok((slippage, 1)); // Default to 1 hop on fallback
                                                }
                                            }
                                            val
                                        },
                                        Err(e) => {
                                            log::warn!(
                                                "⚠️  Jupiter API: failed to parse priceImpactPct '{}': {} - using fallback",
                                                price_impact_str,
                                                e
                                            );
                                            if attempt < MAX_RETRIES {
                                                let backoff = get_backoff(attempt);
                                                log::debug!("Jupiter API: waiting {:?} before retry (attempt {})", backoff, attempt);
                                                tokio::time::sleep(backoff).await;
                                                continue;
                                            } else {
                                                let slippage = self.estimate_with_multipliers(amount)?;
                                    return Ok((slippage, 1)); // Default to 1 hop on fallback
                                            }
                                        }
                                    };
                                    
                                    // ✅ Convert percentage to basis points
                                    // "0.5" (0.5%) → 50 bps
                                    // "1.25" (1.25%) → 125 bps
                                    let slippage_bps = (price_impact_pct * 100.0) as u16;
                                    
                                    // ✅ Calculate hop count from route plan
                                    // Each element in routePlan represents one hop
                                    // Example: USDC -> SOL -> ETH has 2 hops (routePlan.length = 2)
                                    let hop_count = quote.route_plan.as_ref()
                                        .map(|route| route.len() as u8)
                                        .unwrap_or(1); // Default to 1 hop if route plan is missing
                                    
                                    // Validate result is reasonable (should be < 10,000 bps = < 100%)
                                    if slippage_bps > 10_000 {
                                        log::warn!(
                                            "⚠️  Jupiter API returned suspiciously high price impact: '{}' ({}% = {} bps > 100%) - capping at max_slippage_bps",
                                            price_impact_str,
                                            price_impact_pct,
                                            slippage_bps
                                        );
                                        return Ok((self.config.max_slippage_bps, hop_count));
                                    }
                                    
                                    log::info!(
                                        "✅ Jupiter API: priceImpactPct='{}' ({}%) → {} bps, hop_count={}",
                                        price_impact_str,
                                        price_impact_pct,
                                        slippage_bps,
                                        hop_count
                                    );
                                    
                                    return Ok((slippage_bps.min(self.config.max_slippage_bps), hop_count));
                                } else {
                                    log::warn!(
                                        "Jupiter API did not return price impact (attempt {}), will retry",
                                        attempt
                                    );
                                    if attempt < MAX_RETRIES {
                                        let backoff = get_backoff(attempt);
                                        log::debug!("Jupiter API: waiting {:?} before retry (attempt {})", backoff, attempt);
                                        tokio::time::sleep(backoff).await;
                                        continue;
                                    } else {
                                        log::warn!(
                                            "Jupiter API failed after {} attempts, using fallback",
                                            MAX_RETRIES
                                        );
                                        let slippage = self.estimate_with_multipliers(amount)?;
                                    return Ok((slippage, 1)); // Default to 1 hop on fallback
                                    }
                                }
                            }
                            Err(e) => {
                                log::warn!(
                                    "Jupiter API: failed to parse response (attempt {}): {}",
                                    attempt,
                                    e
                                );
                                if attempt < MAX_RETRIES {
                                    let backoff = get_backoff(attempt);
                                    log::debug!("Jupiter API: waiting {:?} before retry (attempt {})", backoff, attempt);
                                    tokio::time::sleep(backoff).await;
                                    continue;
                                } else {
                                    log::warn!(
                                        "Jupiter API failed after {} attempts, using fallback",
                                        MAX_RETRIES
                                    );
                                    let slippage = self.estimate_with_multipliers(amount)?;
                                    return Ok((slippage, 1)); // Default to 1 hop on fallback
                                }
                            }
                        }
                    } else {
                        // HTTP error status
                        let response_text = response.text().await.unwrap_or_default();
                        log::warn!(
                            "Jupiter API error (attempt {}): Status {}, Response: {}",
                            attempt,
                            status,
                            response_text
                        );
                        
                        if attempt < MAX_RETRIES {
                            let backoff = get_backoff(attempt);
                            log::debug!("Jupiter API: waiting {:?} before retry (attempt {})", backoff, attempt);
                            tokio::time::sleep(backoff).await;
                            continue;
                        } else {
                            log::warn!(
                                "Jupiter API failed after {} attempts, using fallback",
                                MAX_RETRIES
                            );
                            return self.estimate_with_multipliers(amount);
                        }
                    }
                }
                Err(e) => {
                    // Network error
                    log::warn!(
                        "Jupiter API network error (attempt {}): {}",
                        attempt,
                        e
                    );
                    
                    if attempt < MAX_RETRIES {
                        let backoff = get_backoff(attempt);
                        log::debug!("Jupiter API: waiting {:?} before retry (attempt {})", backoff, attempt);
                        tokio::time::sleep(backoff).await;
                        continue;
                    } else {
                        log::warn!(
                            "Jupiter API failed after {} attempts, using fallback",
                            MAX_RETRIES
                        );
                        return self.estimate_with_multipliers(amount);
                    }
                }
            }
        }

        // Should never reach here, but fallback just in case
        log::warn!("Jupiter API failed after {} attempts, using fallback", MAX_RETRIES);
        self.estimate_with_multipliers(amount)
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
        use crate::protocol::oracle::{get_pyth_oracle_account, read_pyth_price};
        use crate::blockchain::rpc_client::RpcClient;
        use std::sync::Arc;
        
        let rpc = Arc::new(
            RpcClient::new(self.config.rpc_http_url.clone())
                .context("Failed to create RPC client for oracle confidence")?
        );
        
        if let Some(oracle_account) = get_pyth_oracle_account(&mint, Some(&self.config))? {
            if let Some(price_data) = read_pyth_price(&oracle_account, Arc::clone(&rpc), Some(&self.config)).await? {
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
