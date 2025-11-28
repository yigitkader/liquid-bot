use anyhow::{Context, Result};
use std::env;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Config {
    pub rpc_http_url: String,
    pub rpc_ws_url: String,
    pub wallet_path: String,
    pub hf_liquidation_threshold: f64,
    pub min_profit_usd: f64,
    pub max_slippage_bps: u16,
    pub poll_interval_ms: u64,
    pub dry_run: bool,
    // Protocol configuration
    pub solend_program_id: String,
    pub pyth_program_id: String,
    pub switchboard_program_id: String,
    // Fee configuration
    pub priority_fee_per_cu: u64,
    pub base_transaction_fee_lamports: u64,
    pub dex_fee_bps: u16,
    pub min_profit_margin_bps: u16,
    pub default_oracle_confidence_slippage_bps: u16,
    // Slippage final multiplier: Applied to total slippage (DEX + Oracle) as a safety margin
    // Default: 1.1 (10% safety margin) to account for model uncertainty
    // This is applied after summing DEX slippage and oracle confidence intervals
    pub slippage_final_multiplier: f64,
    // Wallet configuration
    pub min_reserve_lamports: u64,
    // Known reserve addresses (for testing/validation)
    pub usdc_reserve_address: Option<String>,
    pub sol_reserve_address: Option<String>,
    // Associated Token Program ID (standard Solana program)
    pub associated_token_program_id: String,
    // SOL price fallback (only used when oracle fails - should be updated regularly)
    pub sol_price_fallback_usd: f64,
    // Oracle mappings (mint -> oracle account) - optional, falls back to hardcoded defaults
    // Format: JSON string with {"mint": {"pyth": "oracle_account", "switchboard": "oracle_account"}}
    // Example: {"EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v": {"pyth": "5SSkXsEKQepHHAewytPVwdej4epE1h4EmHtUxJ9rKT98", "switchboard": "Gnt27xtC473ZT2Mw5u8wZ68Z3gULkSTb5DuxJy7eJotD"}}
    // If not provided, uses hardcoded defaults for USDC, USDT, SOL, ETH, BTC
    pub oracle_mappings_json: Option<String>,
    // Oracle configuration
    pub max_oracle_age_seconds: u64,
    // ❌ DEPRECATED: Oracle read fee doesn't exist in Solana
    // Solana's base transaction fee covers all account reads, including oracle accounts
    // These fields are kept for backward compatibility but are no longer used in calculations
    pub oracle_read_fee_lamports: u64,
    pub oracle_accounts_read: u64,
    // Transaction configuration
    pub liquidation_compute_units: u32,
    // Statistical configuration
    pub z_score_95: f64, // Z-score for 95% confidence interval (1.96)
    // Slippage size-based multipliers
    pub slippage_size_small_threshold_usd: f64,
    pub slippage_size_large_threshold_usd: f64,
    pub slippage_multiplier_small: f64,
    pub slippage_multiplier_medium: f64,
    pub slippage_multiplier_large: f64,
    // Slippage estimation multiplier (for strategist)
    pub slippage_estimation_multiplier: f64,
    // Transaction lock configuration
    pub tx_lock_timeout_seconds: u64,
    // Retry configuration
    pub max_retries: u32,
    pub initial_retry_delay_ms: u64,
    // Compute budget configuration
    pub default_compute_units: u32,
    pub default_priority_fee_per_cu: u64,
    // WebSocket/Worker sleep configuration
    pub ws_listener_sleep_seconds: u64,
    // RPC polling configuration
    pub max_consecutive_errors: u32,
    // Reserve validation configuration
    pub expected_reserve_size: usize,
    // Liquidation parameters (Solend protocol defaults)
    pub liquidation_bonus: f64,
    pub close_factor: f64,
    pub max_liquidation_slippage: f64,
    // Event bus configuration
    pub event_bus_buffer_size: usize,
    // Health manager configuration
    pub health_manager_max_error_age_seconds: u64,
    // Retry jitter configuration
    pub retry_jitter_max_ms: u64,
    // Jupiter API configuration
    pub use_jupiter_api: bool, // Enable real-time slippage estimation from Jupiter API
    // Note: WebSocket is now the default data source. RPC polling is used as fallback only.
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
            min_profit_usd: env::var("MIN_PROFIT_USD")
                .unwrap_or_else(|_| "5.0".to_string()) // Default: $5 (production-safe)
                .parse()
                .context("Invalid MIN_PROFIT_USD value")?,
            max_slippage_bps: env::var("MAX_SLIPPAGE_BPS")
                .unwrap_or_else(|_| "50".to_string())
                .parse()
                .context("Invalid MAX_SLIPPAGE_BPS value")?,
            // RPC polling interval:
            // - Default increased to 30000ms (30s) to be more friendly to free/public RPC endpoints
            // - WebSocket is primary data source; RPC polling is only a fallback, so higher interval is fine
            poll_interval_ms: env::var("POLL_INTERVAL_MS")
                .unwrap_or_else(|_| "30000".to_string()) // Default: 30s (safer for free RPC endpoints)
                .parse()
                .context("Invalid POLL_INTERVAL_MS value")?,
            dry_run: env::var("DRY_RUN")
                .unwrap_or_else(|_| "true".to_string())
                .parse()
                .context("Invalid DRY_RUN value (must be 'true' or 'false')")?,
            // Protocol IDs - defaults from mainnet
            solend_program_id: env::var("SOLEND_PROGRAM_ID")
                .unwrap_or_else(|_| "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo".to_string()),
            pyth_program_id: env::var("PYTH_PROGRAM_ID")
                .unwrap_or_else(|_| "FsJ3A3u2vn5cTVofAjvy6y5kwABJAqYWpe4975bi2epH".to_string()),
            switchboard_program_id: env::var("SWITCHBOARD_PROGRAM_ID")
                .unwrap_or_else(|_| "SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f".to_string()),
            // Fee configuration - defaults based on Solana mainnet
            priority_fee_per_cu: env::var("PRIORITY_FEE_PER_CU")
                .unwrap_or_else(|_| "1000".to_string()) // 1000 micro-lamports per CU
                .parse()
                .context("Invalid PRIORITY_FEE_PER_CU value")?,
            base_transaction_fee_lamports: env::var("BASE_TRANSACTION_FEE_LAMPORTS")
                .unwrap_or_else(|_| "5000".to_string()) // ~0.000005 SOL (Solana base fee)
                .parse()
                .context("Invalid BASE_TRANSACTION_FEE_LAMPORTS value")?,
            // DEX fee: Typical DEX fees on Solana
            // - Jupiter: 0.1-0.3% (10-30 bps) depending on route
            // - Raydium: 0.25% (25 bps) for most pools
            // - Orca: 0.3% (30 bps) for most pools
            // Default: 0.2% (20 bps) - conservative estimate for Jupiter/Raydium
            // Reference: https://jup.ag/docs/apis/fee-structure
            dex_fee_bps: env::var("DEX_FEE_BPS")
                .unwrap_or_else(|_| "20".to_string()) // 0.2% (typical for Jupiter/Raydium)
                .parse()
                .context("Invalid DEX_FEE_BPS value")?,
            // Minimum profit margin: Ensures we have a buffer above transaction costs
            // Default: 1% (100 bps) of debt amount
            // This prevents marginal opportunities that may become unprofitable due to:
            // - Price movements between detection and execution
            // - Slippage variations
            // - Oracle confidence intervals
            min_profit_margin_bps: env::var("MIN_PROFIT_MARGIN_BPS")
                .unwrap_or_else(|_| "100".to_string()) // 1% minimum profit margin
                .parse()
                .context("Invalid MIN_PROFIT_MARGIN_BPS value")?,
            // Default oracle confidence slippage: Used when oracle price is unavailable
            // This represents the uncertainty in price when we can't read oracle confidence
            // Default: 1% (100 bps) - conservative estimate
            // Note: When oracle is available, we use actual confidence interval (95% with Z-score 1.96)
            default_oracle_confidence_slippage_bps: env::var("DEFAULT_ORACLE_CONFIDENCE_SLIPPAGE_BPS")
                .unwrap_or_else(|_| "100".to_string()) // 1% default when oracle unavailable
                .parse()
                .context("Invalid DEFAULT_ORACLE_CONFIDENCE_SLIPPAGE_BPS value")?,
            slippage_final_multiplier: env::var("SLIPPAGE_FINAL_MULTIPLIER")
                .unwrap_or_else(|_| "1.1".to_string()) // 10% safety margin for model uncertainty
                .parse()
                .context("Invalid SLIPPAGE_FINAL_MULTIPLIER value")?,
            // Wallet configuration
            min_reserve_lamports: env::var("MIN_RESERVE_LAMPORTS")
                .unwrap_or_else(|_| "1000000".to_string()) // 0.001 SOL minimum reserve for transaction fees
                .parse()
                .context("Invalid MIN_RESERVE_LAMPORTS value")?,
            // Known reserve addresses (optional, for testing/validation)
            // Defaults are mainnet Solend reserves
            usdc_reserve_address: env::var("USDC_RESERVE_ADDRESS")
                .ok()
                .or_else(|| Some("BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw".to_string())),
            sol_reserve_address: env::var("SOL_RESERVE_ADDRESS")
                .ok()
                .or_else(|| Some("8PbodeaosQP19SjYFx855UMqWxH2HynZLdBXmsrbac36".to_string())),
            // Associated Token Program ID - standard Solana program (rarely changes)
            associated_token_program_id: env::var("ASSOCIATED_TOKEN_PROGRAM_ID")
                .unwrap_or_else(|_| "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL".to_string()),
            // SOL price fallback - only used when oracle fails (should be updated regularly)
            // ⚠️ WARNING: This is a fallback value. Oracle should be used in production.
            // Update this value regularly to reflect current SOL price.
            sol_price_fallback_usd: env::var("SOL_PRICE_FALLBACK_USD")
                .unwrap_or_else(|_| "150.0".to_string())
                .parse()
                .context("Invalid SOL_PRICE_FALLBACK_USD value")?,
            // Oracle mappings: Optional JSON string with mint -> oracle account mappings
            // If not provided, uses hardcoded defaults (USDC, USDT, SOL, ETH, BTC)
            // This allows users to add custom oracle mappings or update existing ones
            oracle_mappings_json: env::var("ORACLE_MAPPINGS_JSON").ok(),
            // Oracle configuration
            max_oracle_age_seconds: env::var("MAX_ORACLE_AGE_SECONDS")
                .unwrap_or_else(|_| "60".to_string())
                .parse()
                .context("Invalid MAX_ORACLE_AGE_SECONDS value (must be u64)")?,
            // ❌ DEPRECATED: Oracle read fee doesn't exist in Solana
            // Solana's base transaction fee covers all account reads, including oracle accounts
            // These fields are kept for backward compatibility but are no longer used in calculations
            // Reference: https://docs.solana.com/developing/programming-model/runtime#transaction-fees
            oracle_read_fee_lamports: env::var("ORACLE_READ_FEE_LAMPORTS")
                .unwrap_or_else(|_| "5000".to_string())
                .parse()
                .context("Invalid ORACLE_READ_FEE_LAMPORTS value")?,
            oracle_accounts_read: env::var("ORACLE_ACCOUNTS_READ")
                .unwrap_or_else(|_| "1".to_string())
                .parse()
                .context("Invalid ORACLE_ACCOUNTS_READ value")?,
            // Transaction configuration
            liquidation_compute_units: env::var("LIQUIDATION_COMPUTE_UNITS")
                .unwrap_or_else(|_| "200000".to_string())
                .parse()
                .context("Invalid LIQUIDATION_COMPUTE_UNITS value")?,
            // Statistical configuration
            z_score_95: env::var("Z_SCORE_95")
                .unwrap_or_else(|_| "1.96".to_string())
                .parse()
                .context("Invalid Z_SCORE_95 value")?,
            // Slippage size-based multipliers
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
            // Slippage estimation multiplier (for strategist)
            slippage_estimation_multiplier: env::var("SLIPPAGE_ESTIMATION_MULTIPLIER")
                .unwrap_or_else(|_| "0.5".to_string())
                .parse()
                .context("Invalid SLIPPAGE_ESTIMATION_MULTIPLIER value")?,
            // Transaction lock configuration
            tx_lock_timeout_seconds: env::var("TX_LOCK_TIMEOUT_SECONDS")
                .unwrap_or_else(|_| "60".to_string())
                .parse()
                .context("Invalid TX_LOCK_TIMEOUT_SECONDS value")?,
            // Retry configuration
            max_retries: env::var("MAX_RETRIES")
                .unwrap_or_else(|_| "3".to_string())
                .parse()
                .context("Invalid MAX_RETRIES value")?,
            initial_retry_delay_ms: env::var("INITIAL_RETRY_DELAY_MS")
                .unwrap_or_else(|_| "1000".to_string())
                .parse()
                .context("Invalid INITIAL_RETRY_DELAY_MS value")?,
            // Compute budget configuration
            default_compute_units: env::var("DEFAULT_COMPUTE_UNITS")
                .unwrap_or_else(|_| "200000".to_string())
                .parse()
                .context("Invalid DEFAULT_COMPUTE_UNITS value")?,
            default_priority_fee_per_cu: env::var("DEFAULT_PRIORITY_FEE_PER_CU")
                .unwrap_or_else(|_| "1000".to_string())
                .parse()
                .context("Invalid DEFAULT_PRIORITY_FEE_PER_CU value")?,
            // WebSocket/Worker sleep configuration
            ws_listener_sleep_seconds: env::var("WS_LISTENER_SLEEP_SECONDS")
                .unwrap_or_else(|_| "60".to_string())
                .parse()
                .context("Invalid WS_LISTENER_SLEEP_SECONDS value")?,
            // RPC polling configuration
            // Increase default max_consecutive_errors to be more tolerant of transient
            // rate limit errors on free/public RPC endpoints.
            max_consecutive_errors: env::var("MAX_CONSECUTIVE_ERRORS")
                .unwrap_or_else(|_| "30".to_string())
                .parse()
                .context("Invalid MAX_CONSECUTIVE_ERRORS value")?,
            // Reserve validation configuration
            expected_reserve_size: env::var("EXPECTED_RESERVE_SIZE")
                .unwrap_or_else(|_| "619".to_string())
                .parse()
                .context("Invalid EXPECTED_RESERVE_SIZE value")?,
            // Liquidation parameters (Solend protocol defaults)
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
            // Event bus configuration
            event_bus_buffer_size: env::var("EVENT_BUS_BUFFER_SIZE")
                .unwrap_or_else(|_| "1000".to_string())
                .parse()
                .context("Invalid EVENT_BUS_BUFFER_SIZE value")?,
            // Health manager configuration
            health_manager_max_error_age_seconds: env::var("HEALTH_MANAGER_MAX_ERROR_AGE_SECONDS")
                .unwrap_or_else(|_| "300".to_string())
                .parse()
                .context("Invalid HEALTH_MANAGER_MAX_ERROR_AGE_SECONDS value")?,
            // Retry jitter configuration
            retry_jitter_max_ms: env::var("RETRY_JITTER_MAX_MS")
                .unwrap_or_else(|_| "1000".to_string())
                .parse()
                .context("Invalid RETRY_JITTER_MAX_MS value")?,
            // Jupiter API configuration
            // Default: enabled (safest - uses real-time slippage from Jupiter)
            // If you want to use the estimated slippage model instead, explicitly set USE_JUPITER_API=false
            use_jupiter_api: env::var("USE_JUPITER_API")
                .unwrap_or_else(|_| "true".to_string())
                .parse()
                .unwrap_or(true),
            // Note: WebSocket is now always used as primary data source (best practice)
            // RPC polling is used as automatic fallback if WebSocket fails
        };

        config.validate()?;

        Ok(config)
    }

    /// Detects if the RPC endpoint is a free/public endpoint
    /// Free endpoints have strict rate limits (1 req/10s for getProgramAccounts)
    pub fn is_free_rpc_endpoint(&self) -> bool {
        let url_lower = self.rpc_http_url.to_lowercase();
        // Common free/public Solana RPC endpoints
        url_lower.contains("api.mainnet-beta.solana.com")
            || url_lower.contains("api.devnet.solana.com")
            || url_lower.contains("api.testnet.solana.com")
            || url_lower.contains("solana-api.projectserum.com")
    }

    /// Detects if the RPC endpoint is a premium provider
    /// Premium providers typically have higher rate limits or no limits
    pub fn is_premium_rpc_endpoint(&self) -> bool {
        let url_lower = self.rpc_http_url.to_lowercase();
        // Common premium RPC providers
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

        if self.min_profit_usd < 0.0 {
            return Err(anyhow::anyhow!(
                "MIN_PROFIT_USD must be >= 0.0, got: {}",
                self.min_profit_usd
            ));
        }

        if !self.dry_run && self.min_profit_usd < 5.0 {
            log::warn!(
                "⚠️  MIN_PROFIT_USD={} is very low for production! \
                 Transaction fees + gas typically cost $0.1-0.5, \
                 so you may end up with negative profit. \
                 Recommended: MIN_PROFIT_USD >= 5.0 for production, \
                 MIN_PROFIT_USD=1.0 only for testing.",
                self.min_profit_usd
            );
        } else if self.min_profit_usd < 1.0 {
            log::warn!(
                "⚠️  MIN_PROFIT_USD={} is very low! \
                 Recommended: MIN_PROFIT_USD >= 5.0 for production, \
                 MIN_PROFIT_USD=1.0 only for testing.",
                self.min_profit_usd
            );
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

        // ⚠️ getProgramAccounts rate limiting uyarısı
        // WebSocket artık varsayılan, ama RPC polling fallback olarak kullanılabilir
        // Fallback durumunda rate limiting kontrolü yap
        let is_free_rpc = self.is_free_rpc_endpoint();
        let is_premium_rpc = self.is_premium_rpc_endpoint();
        
        if is_free_rpc && self.poll_interval_ms < 10000 {
            log::error!(
                "🚨 OPERASYONEL RİSK: Free RPC endpoint + kısa polling interval!"
            );
            log::error!(
                "   RPC: {} (ücretsiz endpoint tespit edildi)",
                self.rpc_http_url
            );
            log::error!(
                "   POLL_INTERVAL_MS: {}ms (önerilen: 10000ms minimum)",
                self.poll_interval_ms
            );
            log::error!(
                "   Free RPC'ler getProgramAccounts için 1 req/10s limit koyar!"
            );
            log::error!("");
            log::error!("   ÇÖZÜM SEÇENEKLERİ:");
            log::error!("   1. Polling interval'ı artır: POLL_INTERVAL_MS=10000");
            log::error!("   2. WebSocket kullanılacak (varsayılan, önerilen)");
            log::error!("   3. Premium RPC kullan: Helius, Triton, QuickNode");
            log::error!("");
        } else if self.poll_interval_ms < 10000 {
            log::warn!(
                "⚠️  POLL_INTERVAL_MS={}ms is very short for getProgramAccounts!",
                self.poll_interval_ms
            );
            if !is_premium_rpc {
                log::warn!(
                    "⚠️  Recommended: POLL_INTERVAL_MS=10000 (10s) minimum for RPC polling fallback"
                );
            } else {
                log::warn!(
                    "⚠️  Premium RPC detected, but still recommend POLL_INTERVAL_MS>=5000ms for getProgramAccounts"
                );
            }
        }
        
        // Free RPC + kısa polling interval uyarısı (fallback durumu için)
        if is_free_rpc {
            log::warn!("");
            log::warn!("💡 NOT: WebSocket varsayılan olarak kullanılacak.");
            log::warn!("   Eğer WebSocket başarısız olursa RPC polling fallback olarak devreye girecek.");
            log::warn!("   Fallback durumunda free RPC + kısa polling interval sorun yaratabilir.");
            log::warn!("");
        }
        
        // WebSocket varsayılan olarak kullanılacak (best practice)
        log::info!("✅ WebSocket will be used as primary data source (best practice)");
        log::info!("   - Real-time updates (<100ms latency)");
        log::info!("   - No rate limits");
        log::info!("   - RPC polling will be used as fallback if WebSocket fails");

        if !self.dry_run {
            log::warn!("⚠️  DRY_RUN=false: Bot will send REAL transactions to blockchain!");
            log::warn!("⚠️  Make sure you have tested thoroughly in dry-run mode!");
        }

        // Validate protocol IDs format (should be base58 encoded pubkeys)
        if self.solend_program_id.len() < 32 || self.solend_program_id.len() > 44 {
            log::warn!("⚠️  SOLEND_PROGRAM_ID length seems unusual: {} (expected 32-44 chars)", self.solend_program_id.len());
        }
        if self.pyth_program_id.len() < 32 || self.pyth_program_id.len() > 44 {
            log::warn!("⚠️  PYTH_PROGRAM_ID length seems unusual: {} (expected 32-44 chars)", self.pyth_program_id.len());
        }
        if self.switchboard_program_id.len() < 32 || self.switchboard_program_id.len() > 44 {
            log::warn!("⚠️  SWITCHBOARD_PROGRAM_ID length seems unusual: {} (expected 32-44 chars)", self.switchboard_program_id.len());
        }

        // Validate fee configuration
        if self.priority_fee_per_cu == 0 {
            log::warn!("⚠️  PRIORITY_FEE_PER_CU is 0, transactions may be slow or fail");
        }
        if self.dex_fee_bps > 1000 {
            log::warn!("⚠️  DEX_FEE_BPS={} is very high (>10%), double-check this value", self.dex_fee_bps);
        }

        // Slippage calibration warning
        if !self.use_jupiter_api {
            log::warn!("");
            log::warn!("⚠️  SLIPPAGE CALIBRATION REQUIRED: Jupiter API is disabled (USE_JUPITER_API=false)");
            log::warn!("   Using ESTIMATED slippage multipliers - these MUST be calibrated in production!");
            log::warn!("   ");
            log::warn!("   After first 10-20 liquidations:");
            log::warn!("   1. Measure actual slippage from Solscan transactions");
            log::warn!("   2. Compare with estimated slippage for different trade sizes");
            log::warn!("   3. Adjust multipliers: SLIPPAGE_MULTIPLIER_SMALL/MEDIUM/LARGE");
            log::warn!("   ");
            log::warn!("   See docs/SLIPPAGE_CALIBRATION.md for detailed instructions");
            log::warn!("   ");
            log::warn!("   RECOMMENDED: Enable Jupiter API (USE_JUPITER_API=true) for real-time slippage");
            log::warn!("");
        } else {
            log::info!("✅ Jupiter API enabled - using real-time slippage estimation");
        }

        Ok(())
    }
}
