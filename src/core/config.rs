use anyhow::{Context, Result};
use std::env;
use std::path::Path;
use crate::core::registry::{ProgramIds, MintAddresses, ReserveAddresses, LendingMarketAddresses};

#[derive(Debug, Clone)]
pub struct Config {
    pub rpc_http_url: String,
    pub rpc_ws_url: String,
    pub wallet_path: String,
    pub hf_liquidation_threshold: f64,
    pub liquidation_safety_margin: f64, // Safety margin for liquidation threshold (default: 0.95)
    pub min_profit_usd: f64,
    pub max_slippage_bps: u16,
    pub poll_interval_ms: u64,
    pub dry_run: bool,
    pub solend_program_id: String,
    pub pyth_program_id: String,
    pub switchboard_program_id: String,
    pub priority_fee_per_cu: u64,
    pub base_transaction_fee_lamports: u64,
    pub dex_fee_bps: u16,
    pub min_profit_margin_bps: u16,
    pub default_oracle_confidence_slippage_bps: u16,
    pub slippage_final_multiplier: f64,
    pub min_reserve_lamports: u64,
    pub usdc_reserve_address: Option<String>,
    pub sol_reserve_address: Option<String>,
    pub associated_token_program_id: String,
    pub sol_price_fallback_usd: f64,
    pub oracle_mappings_json: Option<String>,
    pub max_oracle_age_seconds: u64,
    pub oracle_read_fee_lamports: u64,
    pub oracle_accounts_read: u64,
    pub liquidation_compute_units: u32,
    pub z_score_95: f64, // Z-score for 95% confidence interval (1.96)
    pub slippage_size_small_threshold_usd: f64,
    pub slippage_size_large_threshold_usd: f64,
    pub slippage_multiplier_small: f64,
    pub slippage_multiplier_medium: f64,
    pub slippage_multiplier_large: f64,
    pub slippage_estimation_multiplier: f64,
    pub tx_lock_timeout_seconds: u64,
    pub max_retries: u32,
    pub initial_retry_delay_ms: u64,
    pub default_compute_units: u32,
    pub default_priority_fee_per_cu: u64,
    pub ws_listener_sleep_seconds: u64,
    pub max_consecutive_errors: u32,
    pub expected_reserve_size: usize,
    pub liquidation_bonus: f64,
    pub close_factor: f64,
    pub max_liquidation_slippage: f64,
    pub event_bus_buffer_size: usize,
    pub analyzer_max_workers: usize,
    pub analyzer_max_workers_limit: usize,
    pub health_manager_max_error_age_seconds: u64,
    pub retry_jitter_max_ms: u64,
    pub use_jupiter_api: bool,
    pub slippage_calibration_file: Option<String>,
    pub slippage_min_measurements_per_category: usize,
    pub main_lending_market_address: Option<String>,
    pub test_wallet_pubkey: Option<String>,
    pub usdc_mint: String,
    pub sol_mint: String,
    pub usdt_mint: Option<String>,
    pub eth_mint: Option<String>,
    pub btc_mint: Option<String>,
    pub default_pyth_oracle_mappings_json: Option<String>,
    pub default_switchboard_oracle_mappings_json: Option<String>,
    pub unwrap_wsol_after_liquidation: bool, // Optional: Unwrap WSOL to native SOL after liquidation
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let config = Config {
            rpc_http_url: env::var("RPC_HTTP_URL")
                .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string()),
            rpc_ws_url: env::var("RPC_WS_URL")
                .unwrap_or_else(|_| "wss://api.mainnet-beta.solana.com".to_string()),
            wallet_path: env::var("WALLET_PATH")
                .unwrap_or_else(|_| "./secret/bot-wallet.json".to_string()),
            hf_liquidation_threshold: env::var("HF_LIQUIDATION_THRESHOLD")
                .unwrap_or_else(|_| "1.0".to_string())
                .parse()
                .context("Invalid HF_LIQUIDATION_THRESHOLD value")?,
            liquidation_safety_margin: env::var("LIQUIDATION_SAFETY_MARGIN")
                .unwrap_or_else(|_| "0.95".to_string()) // Default: 0.95 (5% safety margin)
                .parse()
                .context("Invalid LIQUIDATION_SAFETY_MARGIN value (must be between 0.0 and 1.0)")?,
            min_profit_usd: env::var("MIN_PROFIT_USD")
                .unwrap_or_else(|_| "5.0".to_string()) // Default: $5 (production-safe)
                .parse()
                .context("Invalid MIN_PROFIT_USD value")?,
            max_slippage_bps: env::var("MAX_SLIPPAGE_BPS")
                .unwrap_or_else(|_| "50".to_string())
                .parse()
                .context("Invalid MAX_SLIPPAGE_BPS value")?,
            poll_interval_ms: env::var("POLL_INTERVAL_MS")
                .unwrap_or_else(|_| "30000".to_string()) // Default: 30s (safer for free RPC endpoints)
                .parse()
                .context("Invalid POLL_INTERVAL_MS value")?,
            dry_run: env::var("DRY_RUN")
                .unwrap_or_else(|_| "true".to_string())
                .parse()
                .context("Invalid DRY_RUN value (must be 'true' or 'false')")?,
            solend_program_id: env::var("SOLEND_PROGRAM_ID")
                .unwrap_or_else(|_| ProgramIds::SOLEND.to_string()),
            pyth_program_id: env::var("PYTH_PROGRAM_ID")
                .unwrap_or_else(|_| ProgramIds::PYTH.to_string()),
            switchboard_program_id: env::var("SWITCHBOARD_PROGRAM_ID")
                .unwrap_or_else(|_| ProgramIds::SWITCHBOARD.to_string()),
            priority_fee_per_cu: env::var("PRIORITY_FEE_PER_CU")
                .unwrap_or_else(|_| "1000".to_string()) // 1000 micro-lamports per CU
                .parse()
                .context("Invalid PRIORITY_FEE_PER_CU value")?,
            base_transaction_fee_lamports: env::var("BASE_TRANSACTION_FEE_LAMPORTS")
                .unwrap_or_else(|_| "5000".to_string()) // ~0.000005 SOL (Solana base fee)
                .parse()
                .context("Invalid BASE_TRANSACTION_FEE_LAMPORTS value")?,
            dex_fee_bps: env::var("DEX_FEE_BPS")
                .unwrap_or_else(|_| "20".to_string()) // 0.2% (typical for Jupiter/Raydium)
                .parse()
                .context("Invalid DEX_FEE_BPS value")?,
            min_profit_margin_bps: env::var("MIN_PROFIT_MARGIN_BPS")
                .unwrap_or_else(|_| "100".to_string()) // 1% minimum profit margin
                .parse()
                .context("Invalid MIN_PROFIT_MARGIN_BPS value")?,
            default_oracle_confidence_slippage_bps: env::var(
                "DEFAULT_ORACLE_CONFIDENCE_SLIPPAGE_BPS",
            )
            .unwrap_or_else(|_| "100".to_string()) // 1% default when oracle unavailable
            .parse()
            .context("Invalid DEFAULT_ORACLE_CONFIDENCE_SLIPPAGE_BPS value")?,
            slippage_final_multiplier: env::var("SLIPPAGE_FINAL_MULTIPLIER")
                .unwrap_or_else(|_| "1.1".to_string()) // 10% safety margin for model uncertainty
                .parse()
                .context("Invalid SLIPPAGE_FINAL_MULTIPLIER value")?,
            min_reserve_lamports: env::var("MIN_RESERVE_LAMPORTS")
                .unwrap_or_else(|_| "1000000".to_string()) // 0.001 SOL minimum reserve for transaction fees
                .parse()
                .context("Invalid MIN_RESERVE_LAMPORTS value")?,
            usdc_reserve_address: env::var("USDC_RESERVE_ADDRESS")
                .ok()
                .or_else(|| Some(ReserveAddresses::USDC.to_string())),
            sol_reserve_address: env::var("SOL_RESERVE_ADDRESS")
                .ok()
                .or_else(|| Some(ReserveAddresses::SOL.to_string())),
            associated_token_program_id: env::var("ASSOCIATED_TOKEN_PROGRAM_ID")
                .unwrap_or_else(|_| ProgramIds::ASSOCIATED_TOKEN.to_string()),
            sol_price_fallback_usd: env::var("SOL_PRICE_FALLBACK_USD")
                .unwrap_or_else(|_| "150.0".to_string())
                .parse()
                .context("Invalid SOL_PRICE_FALLBACK_USD value")?,
            oracle_mappings_json: env::var("ORACLE_MAPPINGS_JSON").ok(),
            max_oracle_age_seconds: env::var("MAX_ORACLE_AGE_SECONDS")
                .unwrap_or_else(|_| "60".to_string())
                .parse()
                .context("Invalid MAX_ORACLE_AGE_SECONDS value (must be u64)")?,
            oracle_read_fee_lamports: env::var("ORACLE_READ_FEE_LAMPORTS")
                .unwrap_or_else(|_| "5000".to_string())
                .parse()
                .context("Invalid ORACLE_READ_FEE_LAMPORTS value")?,
            oracle_accounts_read: env::var("ORACLE_ACCOUNTS_READ")
                .unwrap_or_else(|_| "1".to_string())
                .parse()
                .context("Invalid ORACLE_ACCOUNTS_READ value")?,
            liquidation_compute_units: env::var("LIQUIDATION_COMPUTE_UNITS")
                .unwrap_or_else(|_| "200000".to_string())
                .parse()
                .context("Invalid LIQUIDATION_COMPUTE_UNITS value")?,
            z_score_95: env::var("Z_SCORE_95")
                .unwrap_or_else(|_| "1.96".to_string())
                .parse()
                .context("Invalid Z_SCORE_95 value")?,
            slippage_size_small_threshold_usd: env::var("SLIPPAGE_SIZE_SMALL_THRESHOLD_USD")
                .unwrap_or_else(|_| "10000.0".to_string())
                .parse()
                .context("Invalid SLIPPAGE_SIZE_SMALL_THRESHOLD_USD value")?,
            slippage_size_large_threshold_usd: env::var("SLIPPAGE_SIZE_LARGE_THRESHOLD_USD")
                .unwrap_or_else(|_| "100000.0".to_string())
                .parse()
                .context("Invalid SLIPPAGE_SIZE_LARGE_THRESHOLD_USD value")?,
            slippage_multiplier_small: env::var("SLIPPAGE_MULTIPLIER_SMALL")
                .unwrap_or_else(|_| "0.5".to_string())
                .parse()
                .context("Invalid SLIPPAGE_MULTIPLIER_SMALL value")?,
            slippage_multiplier_medium: env::var("SLIPPAGE_MULTIPLIER_MEDIUM")
                .unwrap_or_else(|_| "0.6".to_string())
                .parse()
                .context("Invalid SLIPPAGE_MULTIPLIER_MEDIUM value")?,
            slippage_multiplier_large: env::var("SLIPPAGE_MULTIPLIER_LARGE")
                .unwrap_or_else(|_| "0.8".to_string())
                .parse()
                .context("Invalid SLIPPAGE_MULTIPLIER_LARGE value")?,
            slippage_estimation_multiplier: env::var("SLIPPAGE_ESTIMATION_MULTIPLIER")
                .unwrap_or_else(|_| "0.5".to_string())
                .parse()
                .context("Invalid SLIPPAGE_ESTIMATION_MULTIPLIER value")?,
            tx_lock_timeout_seconds: env::var("TX_LOCK_TIMEOUT_SECONDS")
                .unwrap_or_else(|_| "60".to_string())
                .parse()
                .context("Invalid TX_LOCK_TIMEOUT_SECONDS value")?,
            max_retries: env::var("MAX_RETRIES")
                .unwrap_or_else(|_| "3".to_string())
                .parse()
                .context("Invalid MAX_RETRIES value")?,
            initial_retry_delay_ms: env::var("INITIAL_RETRY_DELAY_MS")
                .unwrap_or_else(|_| "1000".to_string())
                .parse()
                .context("Invalid INITIAL_RETRY_DELAY_MS value")?,
            default_compute_units: env::var("DEFAULT_COMPUTE_UNITS")
                .unwrap_or_else(|_| "200000".to_string())
                .parse()
                .context("Invalid DEFAULT_COMPUTE_UNITS value")?,
            default_priority_fee_per_cu: env::var("DEFAULT_PRIORITY_FEE_PER_CU")
                .unwrap_or_else(|_| "1000".to_string())
                .parse()
                .context("Invalid DEFAULT_PRIORITY_FEE_PER_CU value")?,
            ws_listener_sleep_seconds: env::var("WS_LISTENER_SLEEP_SECONDS")
                .unwrap_or_else(|_| "60".to_string())
                .parse()
                .context("Invalid WS_LISTENER_SLEEP_SECONDS value")?,
            max_consecutive_errors: env::var("MAX_CONSECUTIVE_ERRORS")
                .unwrap_or_else(|_| "30".to_string())
                .parse()
                .context("Invalid MAX_CONSECUTIVE_ERRORS value")?,
            expected_reserve_size: env::var("EXPECTED_RESERVE_SIZE")
                .unwrap_or_else(|_| "619".to_string())
                .parse()
                .context("Invalid EXPECTED_RESERVE_SIZE value")?,
            liquidation_bonus: env::var("LIQUIDATION_BONUS")
                .unwrap_or_else(|_| "0.05".to_string())
                .parse()
                .context("Invalid LIQUIDATION_BONUS value")?,
            close_factor: env::var("CLOSE_FACTOR")
                .unwrap_or_else(|_| "0.5".to_string())
                .parse()
                .context("Invalid CLOSE_FACTOR value")?,
            max_liquidation_slippage: env::var("MAX_LIQUIDATION_SLIPPAGE")
                .unwrap_or_else(|_| "0.01".to_string())
                .parse()
                .context("Invalid MAX_LIQUIDATION_SLIPPAGE value")?,
            event_bus_buffer_size: env::var("EVENT_BUS_BUFFER_SIZE")
                .unwrap_or_else(|_| "50000".to_string()) // Increased from 10000 to 50000 to prevent lag
                .parse()
                .context("Invalid EVENT_BUS_BUFFER_SIZE value")?,
            analyzer_max_workers: env::var("ANALYZER_MAX_WORKERS")
                .unwrap_or_else(|_| "4".to_string())
                .parse()
                .context("Invalid ANALYZER_MAX_WORKERS value")?,
            analyzer_max_workers_limit: env::var("ANALYZER_MAX_WORKERS_LIMIT")
                .unwrap_or_else(|_| "16".to_string())
                .parse()
                .context("Invalid ANALYZER_MAX_WORKERS_LIMIT value")?,
            health_manager_max_error_age_seconds: env::var("HEALTH_MANAGER_MAX_ERROR_AGE_SECONDS")
                .unwrap_or_else(|_| "300".to_string())
                .parse()
                .context("Invalid HEALTH_MANAGER_MAX_ERROR_AGE_SECONDS value")?,
            retry_jitter_max_ms: env::var("RETRY_JITTER_MAX_MS")
                .unwrap_or_else(|_| "1000".to_string())
                .parse()
                .context("Invalid RETRY_JITTER_MAX_MS value")?,
            use_jupiter_api: env::var("USE_JUPITER_API")
                .unwrap_or_else(|_| "true".to_string())
                .parse()
                .unwrap_or(true),
            slippage_calibration_file: env::var("SLIPPAGE_CALIBRATION_FILE")
                .ok()
                .or_else(|| Some("slippage_calibration.json".to_string())), // Default file
            slippage_min_measurements_per_category: env::var(
                "SLIPPAGE_MIN_MEASUREMENTS_PER_CATEGORY",
            )
            .unwrap_or_else(|_| "10".to_string())
            .parse()
            .context("Invalid SLIPPAGE_MIN_MEASUREMENTS_PER_CATEGORY value")?,
            main_lending_market_address: env::var("MAIN_LENDING_MARKET_ADDRESS")
                .ok()
                .or_else(|| Some(LendingMarketAddresses::MAIN.to_string())),
            test_wallet_pubkey: env::var("TEST_WALLET_PUBKEY")
                .ok()
                .or_else(|| Some("11111111111111111111111111111111".to_string())),
            usdc_mint: env::var("USDC_MINT")
                .unwrap_or_else(|_| MintAddresses::USDC.to_string()),
            sol_mint: env::var("SOL_MINT")
                .unwrap_or_else(|_| MintAddresses::SOL.to_string()),
            usdt_mint: env::var("USDT_MINT")
                .ok()
                .or_else(|| Some(MintAddresses::USDT.to_string())),
            eth_mint: env::var("ETH_MINT")
                .ok()
                .or_else(|| Some(MintAddresses::ETH.to_string())),
            btc_mint: env::var("BTC_MINT")
                .ok()
                .or_else(|| Some(MintAddresses::BTC.to_string())),
            default_pyth_oracle_mappings_json: env::var("DEFAULT_PYTH_ORACLE_MAPPINGS_JSON").ok(),
            default_switchboard_oracle_mappings_json: env::var(
                "DEFAULT_SWITCHBOARD_ORACLE_MAPPINGS_JSON",
            )
            .ok(),
            unwrap_wsol_after_liquidation: env::var("UNWRAP_WSOL_AFTER_LIQUIDATION")
                .unwrap_or_else(|_| "false".to_string())
                .parse()
                .unwrap_or(false), // Default: false (optional feature)
        };

        config.validate()?;

        Ok(config)
    }

    pub fn is_free_rpc_endpoint(&self) -> bool {
        let url_lower = self.rpc_http_url.to_lowercase();
        url_lower.contains("api.mainnet-beta.solana.com")
            || url_lower.contains("api.devnet.solana.com")
            || url_lower.contains("api.testnet.solana.com")
            || url_lower.contains("solana-api.projectserum.com")
    }

    pub fn is_premium_rpc_endpoint(&self) -> bool {
        let url_lower = self.rpc_http_url.to_lowercase();
        url_lower.contains("helius")
            || url_lower.contains("triton")
            || url_lower.contains("quicknode")
            || url_lower.contains("alchemy")
            || url_lower.contains("ankr")
            || url_lower.contains("getblock")
            || url_lower.contains("rpcpool")
    }

    pub fn validate(&self) -> Result<()> {
        if !self.rpc_http_url.starts_with("http://") && !self.rpc_http_url.starts_with("https://") {
            return Err(anyhow::anyhow!(
                "RPC_HTTP_URL must start with http:// or https://"
            ));
        }

        if !self.rpc_ws_url.starts_with("ws://") && !self.rpc_ws_url.starts_with("wss://") {
            return Err(anyhow::anyhow!(
                "RPC_WS_URL must start with ws:// or wss://"
            ));
        }

        if !Path::new(&self.wallet_path).exists() {
            return Err(anyhow::anyhow!(
                "Wallet file not found: {}. Please create wallet first.",
                self.wallet_path
            ));
        }

        if self.hf_liquidation_threshold <= 0.0 || self.hf_liquidation_threshold > 10.0 {
            return Err(anyhow::anyhow!(
                "HF_LIQUIDATION_THRESHOLD must be between 0.0 and 10.0, got: {}",
                self.hf_liquidation_threshold
            ));
        }

        if self.liquidation_safety_margin <= 0.0 || self.liquidation_safety_margin > 1.0 {
            return Err(anyhow::anyhow!(
                "LIQUIDATION_SAFETY_MARGIN must be between 0.0 and 1.0, got: {}",
                self.liquidation_safety_margin
            ));
        }

        // ✅ FIX: Validate slippage_final_multiplier (safety margin for model uncertainty)
        if self.slippage_final_multiplier < 1.0 || self.slippage_final_multiplier > 2.0 {
            return Err(anyhow::anyhow!(
                "SLIPPAGE_FINAL_MULTIPLIER must be between 1.0 and 2.0 (safety margin), got: {}",
                self.slippage_final_multiplier
            ));
        }

        // ✅ FIX: Validate z_score_95 (statistical parameter)
        // Standard value is 1.96 for 95% confidence interval
        if (self.z_score_95 - 1.96).abs() > 0.01 {
            log::warn!(
                "⚠️  Z_SCORE_95={} is not standard (expected: 1.96 for 95% confidence interval)",
                self.z_score_95
            );
            log::warn!("   Using non-standard z-score may lead to incorrect statistical calculations");
        }

        // ✅ FIX: Validate liquidation_bonus (typically 0-20%)
        if self.liquidation_bonus < 0.0 || self.liquidation_bonus > 0.20 {
            return Err(anyhow::anyhow!(
                "LIQUIDATION_BONUS must be between 0.0 and 0.20 (0-20%), got: {}",
                self.liquidation_bonus
            ));
        }

        // RPC Rate Limit Check: Free RPC endpoints require longer polling intervals
        if self.is_free_rpc_endpoint() && self.poll_interval_ms < 10000 {
            return Err(anyhow::anyhow!(
                "FATAL: Free RPC endpoint requires POLL_INTERVAL_MS >= 10000ms (got: {}). \
                 Either use premium RPC or increase polling interval.",
                self.poll_interval_ms
            ));
        }

        if self.min_profit_usd < 0.0 {
            return Err(anyhow::anyhow!(
                "MIN_PROFIT_USD must be >= 0.0, got: {}",
                self.min_profit_usd
            ));
        }

        // MIN_PROFIT_USD validation with clear thresholds:
        // - $2.0: Minimum (error if below in production)
        // - $5.0: Recommended (warning if below in production)
        // - $10.0: Safe (info if at or above in production)

        if !self.dry_run {
            // Production mode: Strict validation
            if self.min_profit_usd < 2.0 {
                return Err(anyhow::anyhow!(
                    "MIN_PROFIT_USD must be >= $2.0 in production (got: ${}). \
                     Transaction fees + gas typically cost $0.1-0.5, and slippage can add more. \
                     Lower values may result in negative profit. \
                     For testing, use DRY_RUN=true with lower MIN_PROFIT_USD values.",
                    self.min_profit_usd
                ));
            } else if self.min_profit_usd < 5.0 {
                // Between $2.0 and $5.0: Warning (below recommended)
                log::warn!(
                    "⚠️  MIN_PROFIT_USD=${} is below recommended $5.0 for production! \
                     Transaction fees + gas typically cost $0.1-0.5, \
                     so you may end up with lower profit margins. \
                     Recommended: MIN_PROFIT_USD >= 5.0 for production.",
                    self.min_profit_usd
                );
            } else if self.min_profit_usd >= 10.0 {
                // $10.0 or above: Safe (info message)
                log::info!(
                    "✅ MIN_PROFIT_USD=${} is safe for production (>= $10.0 recommended)",
                    self.min_profit_usd
                );
            }
        } else {
            // Dry-run mode: More lenient, but still warn if too low
            if self.min_profit_usd < 1.0 {
                log::warn!(
                    "⚠️  MIN_PROFIT_USD=${} is very low even for dry-run testing! \
                     Recommended: MIN_PROFIT_USD >= 1.0 for testing.",
                    self.min_profit_usd
                );
            }
        }

        if self.max_slippage_bps > 10000 {
            return Err(anyhow::anyhow!(
                "MAX_SLIPPAGE_BPS must be <= 10000 (100%), got: {}",
                self.max_slippage_bps
            ));
        }

        if self.poll_interval_ms < 100 {
            return Err(anyhow::anyhow!(
                "POLL_INTERVAL_MS must be >= 100ms, got: {}",
                self.poll_interval_ms
            ));
        }

        let is_free_rpc = self.is_free_rpc_endpoint();
        let is_premium_rpc = self.is_premium_rpc_endpoint();

        // Note: Free RPC + short polling check is now done earlier in validate() as a hard error
        if is_free_rpc && self.poll_interval_ms >= 10000 {
            log::info!(
                "✅ Free RPC endpoint with safe polling interval: {}ms",
                self.poll_interval_ms
            );
        } else if is_premium_rpc {
            log::info!("✅ Premium RPC endpoint detected");
            if self.poll_interval_ms < 10000 {
                log::warn!(
                    "⚠️  POLL_INTERVAL_MS={}ms is short (OK for premium RPC, but >= 10000ms recommended)",
                    self.poll_interval_ms
                );
            }
        } else if self.poll_interval_ms < 10000 {
            log::warn!(
                "⚠️  POLL_INTERVAL_MS={}ms is very short for getProgramAccounts!",
                self.poll_interval_ms
            );
            log::warn!(
                "⚠️  Recommended: POLL_INTERVAL_MS=10000 (10s) minimum for RPC polling fallback"
            );
        }

        if is_free_rpc {
            log::warn!("");
            log::warn!("💡 NOT: WebSocket varsayılan olarak kullanılacak.");
            log::warn!(
                "   Eğer WebSocket başarısız olursa RPC polling fallback olarak devreye girecek."
            );
            log::warn!("   Fallback durumunda free RPC + kısa polling interval sorun yaratabilir.");
            log::warn!("");
        }

        log::info!("✅ WebSocket will be used as primary data source (best practice)");
        log::info!("   - Real-time updates (<100ms latency)");
        log::info!("   - No rate limits");
        log::info!("   - RPC polling will be used as fallback if WebSocket fails");

        if !self.dry_run {
            log::warn!("⚠️  DRY_RUN=false: Bot will send REAL transactions to blockchain!");
            log::warn!("⚠️  Make sure you have tested thoroughly in dry-run mode!");
        }

        if self.solend_program_id.len() < 32 || self.solend_program_id.len() > 44 {
            log::warn!(
                "⚠️  SOLEND_PROGRAM_ID length seems unusual: {} (expected 32-44 chars)",
                self.solend_program_id.len()
            );
        }
        if self.pyth_program_id.len() < 32 || self.pyth_program_id.len() > 44 {
            log::warn!(
                "⚠️  PYTH_PROGRAM_ID length seems unusual: {} (expected 32-44 chars)",
                self.pyth_program_id.len()
            );
        }
        if self.switchboard_program_id.len() < 32 || self.switchboard_program_id.len() > 44 {
            log::warn!(
                "⚠️  SWITCHBOARD_PROGRAM_ID length seems unusual: {} (expected 32-44 chars)",
                self.switchboard_program_id.len()
            );
        }

        if self.priority_fee_per_cu == 0 {
            log::warn!("⚠️  PRIORITY_FEE_PER_CU is 0, transactions may be slow or fail");
        }
        if self.dex_fee_bps > 1000 {
            log::warn!(
                "⚠️  DEX_FEE_BPS={} is very high (>10%), double-check this value",
                self.dex_fee_bps
            );
        }

        if !self.dry_run && !self.use_jupiter_api {
            log::warn!("");
            log::warn!("⚠️  SLIPPAGE CALIBRATION REQUIRED: Jupiter API is disabled (USE_JUPITER_API=false)");
            log::warn!(
                "   Using ESTIMATED slippage multipliers - these MUST be calibrated in production!"
            );
            log::warn!("   ");
            log::warn!("   After first 10-20 liquidations:");
            log::warn!("   1. Measure actual slippage from Solscan transactions");
            log::warn!("   2. Compare with estimated slippage for different trade sizes");
            log::warn!("   3. Adjust multipliers: SLIPPAGE_MULTIPLIER_SMALL/MEDIUM/LARGE");
            log::warn!("   ");
            log::warn!("   See docs/SLIPPAGE_CALIBRATION.md for detailed instructions");
            log::warn!("   ");
            log::warn!(
                "   RECOMMENDED: Enable Jupiter API (USE_JUPITER_API=true) for real-time slippage"
            );
            log::warn!("");
        } else if self.use_jupiter_api {
            log::info!("✅ Jupiter API enabled - using real-time slippage estimation");
        } else {
            log::debug!("Slippage calibration warning skipped (dry-run mode)");
        }

        Ok(())
    }

    /// Get RPC configuration group
    pub fn rpc(&self) -> crate::core::config_groups::RpcConfig {
        crate::core::config_groups::RpcConfig::from(self)
    }

    /// Get trading configuration group
    pub fn trading(&self) -> crate::core::config_groups::TradingConfig {
        crate::core::config_groups::TradingConfig::from(self)
    }

    /// Get protocol configuration group
    pub fn protocol(&self) -> crate::core::config_groups::ProtocolConfig {
        crate::core::config_groups::ProtocolConfig::from(self)
    }

    /// Get oracle configuration group
    pub fn oracle(&self) -> crate::core::config_groups::OracleConfig {
        crate::core::config_groups::OracleConfig::from(self)
    }

    /// Get slippage configuration group
    pub fn slippage(&self) -> crate::core::config_groups::SlippageConfig {
        crate::core::config_groups::SlippageConfig::from(self)
    }

    /// Get transaction configuration group
    pub fn transaction(&self) -> crate::core::config_groups::TransactionConfig {
        crate::core::config_groups::TransactionConfig::from(self)
    }

    /// Get system configuration group
    pub fn system(&self) -> crate::core::config_groups::SystemConfig {
        crate::core::config_groups::SystemConfig::from(self)
    }

    /// Get token configuration group
    pub fn tokens(&self) -> crate::core::config_groups::TokenConfig {
        crate::core::config_groups::TokenConfig::from(self)
    }

    /// Get profit configuration group
    pub fn profit(&self) -> crate::core::config_groups::ProfitConfig {
        crate::core::config_groups::ProfitConfig::from(self)
    }
}
