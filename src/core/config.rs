use anyhow::Result;
use std::env;
use std::path::Path;
use crate::core::registry::{ProgramIds, MintAddresses, ReserveAddresses, LendingMarketAddresses};

macro_rules! env_parse {
    ($key:expr, $default:expr) => {
        env::var($key)
            .unwrap_or_else(|_| $default.to_string())
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid {}: {}", $key, e))?
    };
}

fn env_str(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_opt_str(key: &str) -> Option<String> {
    env::var(key).ok()
}

fn env_opt_with_default(key: &str, default: String) -> Option<String> {
    env_opt_str(key).or(Some(default))
}

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
            rpc_http_url: env_str("RPC_HTTP_URL", "https://api.mainnet-beta.solana.com"),
            rpc_ws_url: env_str("RPC_WS_URL", "wss://api.mainnet-beta.solana.com"),
            wallet_path: env_str("WALLET_PATH", "./secret/bot-wallet.json"),
            hf_liquidation_threshold: env_parse!("HF_LIQUIDATION_THRESHOLD", "1.0"),
            liquidation_safety_margin: env_parse!("LIQUIDATION_SAFETY_MARGIN", "0.95"),
            min_profit_usd: env_parse!("MIN_PROFIT_USD", "5.0"),
            max_slippage_bps: env_parse!("MAX_SLIPPAGE_BPS", "50"),
            poll_interval_ms: env_parse!("POLL_INTERVAL_MS", "30000"),
            dry_run: env_parse!("DRY_RUN", "true"),
            solend_program_id: env_str("SOLEND_PROGRAM_ID", &ProgramIds::SOLEND.to_string()),
            pyth_program_id: env_str("PYTH_PROGRAM_ID", &ProgramIds::PYTH.to_string()),
            switchboard_program_id: env_str("SWITCHBOARD_PROGRAM_ID", &ProgramIds::SWITCHBOARD.to_string()),
            priority_fee_per_cu: env_parse!("PRIORITY_FEE_PER_CU", "1000"),
            base_transaction_fee_lamports: env_parse!("BASE_TRANSACTION_FEE_LAMPORTS", "5000"),
            dex_fee_bps: env_parse!("DEX_FEE_BPS", "20"),
            min_profit_margin_bps: env_parse!("MIN_PROFIT_MARGIN_BPS", "100"),
            default_oracle_confidence_slippage_bps: env_parse!("DEFAULT_ORACLE_CONFIDENCE_SLIPPAGE_BPS", "100"),
            slippage_final_multiplier: env_parse!("SLIPPAGE_FINAL_MULTIPLIER", "1.1"),
            min_reserve_lamports: env_parse!("MIN_RESERVE_LAMPORTS", "1000000"),
            usdc_reserve_address: env_opt_with_default("USDC_RESERVE_ADDRESS", ReserveAddresses::USDC.to_string()),
            sol_reserve_address: env_opt_with_default("SOL_RESERVE_ADDRESS", ReserveAddresses::SOL.to_string()),
            associated_token_program_id: env_str("ASSOCIATED_TOKEN_PROGRAM_ID", &ProgramIds::ASSOCIATED_TOKEN.to_string()),
            sol_price_fallback_usd: env_parse!("SOL_PRICE_FALLBACK_USD", "150.0"),
            oracle_mappings_json: env_opt_str("ORACLE_MAPPINGS_JSON"),
            max_oracle_age_seconds: env_parse!("MAX_ORACLE_AGE_SECONDS", "120"),
            oracle_read_fee_lamports: env_parse!("ORACLE_READ_FEE_LAMPORTS", "5000"),
            oracle_accounts_read: env_parse!("ORACLE_ACCOUNTS_READ", "1"),
            liquidation_compute_units: env_parse!("LIQUIDATION_COMPUTE_UNITS", "200000"),
            z_score_95: env_parse!("Z_SCORE_95", "1.96"),
            slippage_size_small_threshold_usd: env_parse!("SLIPPAGE_SIZE_SMALL_THRESHOLD_USD", "10000.0"),
            slippage_size_large_threshold_usd: env_parse!("SLIPPAGE_SIZE_LARGE_THRESHOLD_USD", "100000.0"),
            slippage_multiplier_small: env_parse!("SLIPPAGE_MULTIPLIER_SMALL", "0.5"),
            slippage_multiplier_medium: env_parse!("SLIPPAGE_MULTIPLIER_MEDIUM", "0.6"),
            slippage_multiplier_large: env_parse!("SLIPPAGE_MULTIPLIER_LARGE", "0.8"),
            slippage_estimation_multiplier: env_parse!("SLIPPAGE_ESTIMATION_MULTIPLIER", "0.5"),
            tx_lock_timeout_seconds: env_parse!("TX_LOCK_TIMEOUT_SECONDS", "60"),
            max_retries: env_parse!("MAX_RETRIES", "3"),
            initial_retry_delay_ms: env_parse!("INITIAL_RETRY_DELAY_MS", "1000"),
            default_compute_units: env_parse!("DEFAULT_COMPUTE_UNITS", "200000"),
            default_priority_fee_per_cu: env_parse!("DEFAULT_PRIORITY_FEE_PER_CU", "1000"),
            ws_listener_sleep_seconds: env_parse!("WS_LISTENER_SLEEP_SECONDS", "60"),
            max_consecutive_errors: env_parse!("MAX_CONSECUTIVE_ERRORS", "30"),
            expected_reserve_size: env_parse!("EXPECTED_RESERVE_SIZE", "619"),
            liquidation_bonus: env_parse!("LIQUIDATION_BONUS", "0.05"),
            close_factor: env_parse!("CLOSE_FACTOR", "0.5"),
            max_liquidation_slippage: env_parse!("MAX_LIQUIDATION_SLIPPAGE", "0.01"),
            event_bus_buffer_size: env_parse!("EVENT_BUS_BUFFER_SIZE", "50000"),
            analyzer_max_workers: env_parse!("ANALYZER_MAX_WORKERS", "4"),
            analyzer_max_workers_limit: env_parse!("ANALYZER_MAX_WORKERS_LIMIT", "16"),
            health_manager_max_error_age_seconds: env_parse!("HEALTH_MANAGER_MAX_ERROR_AGE_SECONDS", "300"),
            retry_jitter_max_ms: env_parse!("RETRY_JITTER_MAX_MS", "1000"),
            use_jupiter_api: env_str("USE_JUPITER_API", "true").parse().unwrap_or(true),
            slippage_calibration_file: env_opt_with_default("SLIPPAGE_CALIBRATION_FILE", "slippage_calibration.json".to_string()),
            slippage_min_measurements_per_category: env_parse!("SLIPPAGE_MIN_MEASUREMENTS_PER_CATEGORY", "10"),
            main_lending_market_address: env_opt_with_default("MAIN_LENDING_MARKET_ADDRESS", LendingMarketAddresses::MAIN.to_string()),
            test_wallet_pubkey: env_opt_with_default("TEST_WALLET_PUBKEY", "11111111111111111111111111111111".to_string()),
            usdc_mint: env_str("USDC_MINT", &MintAddresses::USDC.to_string()),
            sol_mint: env_str("SOL_MINT", &MintAddresses::SOL.to_string()),
            usdt_mint: env_opt_with_default("USDT_MINT", MintAddresses::USDT.to_string()),
            eth_mint: env_opt_with_default("ETH_MINT", MintAddresses::ETH.to_string()),
            btc_mint: env_opt_with_default("BTC_MINT", MintAddresses::BTC.to_string()),
            default_pyth_oracle_mappings_json: env_opt_str("DEFAULT_PYTH_ORACLE_MAPPINGS_JSON"),
            default_switchboard_oracle_mappings_json: env_opt_str("DEFAULT_SWITCHBOARD_ORACLE_MAPPINGS_JSON"),
            unwrap_wsol_after_liquidation: env_str("UNWRAP_WSOL_AFTER_LIQUIDATION", "false").parse().unwrap_or(false),
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

        if !(1.0..=2.0).contains(&self.slippage_final_multiplier) {
            return Err(anyhow::anyhow!("SLIPPAGE_FINAL_MULTIPLIER must be between 1.0 and 2.0, got: {}", self.slippage_final_multiplier));
        }

        if (self.z_score_95 - 1.96).abs() > 0.01 {
            log::warn!("⚠️  Z_SCORE_95={} is not standard (expected: 1.96)", self.z_score_95);
        }

        if !(0.0..=0.20).contains(&self.liquidation_bonus) {
            return Err(anyhow::anyhow!("LIQUIDATION_BONUS must be between 0.0 and 0.20, got: {}", self.liquidation_bonus));
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

        if !self.dry_run {
            if self.min_profit_usd < 2.0 {
                return Err(anyhow::anyhow!(
                    "MIN_PROFIT_USD must be >= $2.0 in production (got: ${}). Use DRY_RUN=true for testing.",
                    self.min_profit_usd
                ));
            } else if self.min_profit_usd < 5.0 {
                log::warn!("⚠️  MIN_PROFIT_USD=${} is below recommended $5.0 for production", self.min_profit_usd);
            } else if self.min_profit_usd >= 10.0 {
                log::info!("✅ MIN_PROFIT_USD=${} is safe for production", self.min_profit_usd);
            }
        } else if self.min_profit_usd < 1.0 {
            log::warn!("⚠️  MIN_PROFIT_USD=${} is very low for testing", self.min_profit_usd);
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

        if is_free_rpc && self.poll_interval_ms >= 10000 {
            log::info!("✅ Free RPC endpoint with safe polling interval: {}ms", self.poll_interval_ms);
        } else if is_premium_rpc {
            log::info!("✅ Premium RPC endpoint detected");
            if self.poll_interval_ms < 10000 {
                log::warn!("⚠️  POLL_INTERVAL_MS={}ms is short (>= 10000ms recommended)", self.poll_interval_ms);
            }
        } else if self.poll_interval_ms < 10000 {
            log::warn!("⚠️  POLL_INTERVAL_MS={}ms is very short for getProgramAccounts (>= 10000ms recommended)", self.poll_interval_ms);
        }

        if is_free_rpc {
            log::warn!("💡 WebSocket will be used as primary. RPC polling fallback may have rate limits.");
        }

        log::info!("✅ WebSocket primary data source (real-time, no rate limits)");

        if !self.dry_run {
            log::warn!("⚠️  DRY_RUN=false: Bot will send REAL transactions to blockchain!");
            log::warn!("⚠️  Make sure you have tested thoroughly in dry-run mode!");
        }

        for (name, id) in [
            ("SOLEND_PROGRAM_ID", &self.solend_program_id),
            ("PYTH_PROGRAM_ID", &self.pyth_program_id),
            ("SWITCHBOARD_PROGRAM_ID", &self.switchboard_program_id),
        ] {
            if id.len() < 32 || id.len() > 44 {
                log::warn!("⚠️  {} length seems unusual: {} (expected 32-44 chars)", name, id.len());
            }
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
            log::warn!("⚠️  SLIPPAGE CALIBRATION REQUIRED: Jupiter API disabled. Estimated slippage multipliers MUST be calibrated in production!");
        } else if self.use_jupiter_api {
            log::info!("✅ Jupiter API enabled - real-time slippage estimation");
        }

        Ok(())
    }

}
