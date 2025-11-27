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
    pub slippage_safety_margin_multiplier: f64,
    // Wallet configuration
    pub min_reserve_lamports: u64,
    // Known reserve addresses (for testing/validation)
    pub usdc_reserve_address: Option<String>,
    pub sol_reserve_address: Option<String>,
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
            poll_interval_ms: env::var("POLL_INTERVAL_MS")
                .unwrap_or_else(|_| "10000".to_string()) // Default: 10s (safe for free RPC endpoints)
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
            dex_fee_bps: env::var("DEX_FEE_BPS")
                .unwrap_or_else(|_| "20".to_string()) // 0.2% (typical for Jupiter/Raydium)
                .parse()
                .context("Invalid DEX_FEE_BPS value")?,
            min_profit_margin_bps: env::var("MIN_PROFIT_MARGIN_BPS")
                .unwrap_or_else(|_| "100".to_string()) // 1% minimum profit margin
                .parse()
                .context("Invalid MIN_PROFIT_MARGIN_BPS value")?,
            default_oracle_confidence_slippage_bps: env::var("DEFAULT_ORACLE_CONFIDENCE_SLIPPAGE_BPS")
                .unwrap_or_else(|_| "100".to_string()) // 1% default when oracle unavailable
                .parse()
                .context("Invalid DEFAULT_ORACLE_CONFIDENCE_SLIPPAGE_BPS value")?,
            slippage_safety_margin_multiplier: env::var("SLIPPAGE_SAFETY_MARGIN_MULTIPLIER")
                .unwrap_or_else(|_| "1.1".to_string()) // 10% safety margin (reduced from 20% to avoid over-conservative profit calculation)
                .parse()
                .context("Invalid SLIPPAGE_SAFETY_MARGIN_MULTIPLIER value")?,
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
        };

        config.validate()?;

        Ok(config)
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
        if self.poll_interval_ms < 10000 {
            log::warn!(
                "⚠️  POLL_INTERVAL_MS={}ms is very short for getProgramAccounts on free RPC endpoints!",
                self.poll_interval_ms
            );
            log::warn!(
                "⚠️  Free RPC endpoints typically limit getProgramAccounts to 1 req/10s"
            );
            log::warn!(
                "⚠️  Recommended: POLL_INTERVAL_MS=10000 (10s) for free RPC, or use premium RPC/WebSocket"
            );
        }

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
        // Note: slippage_safety_margin_multiplier is now hardcoded to 1.1 (10%) in math.rs
        // to avoid over-conservative profit calculations. This config field is kept for
        // backward compatibility but not used in calculations.
        if self.slippage_safety_margin_multiplier < 1.0 || self.slippage_safety_margin_multiplier > 2.0 {
            log::warn!("⚠️  SLIPPAGE_SAFETY_MARGIN_MULTIPLIER={} is outside recommended range (1.0-2.0), but note: this value is no longer used (hardcoded to 1.1)", self.slippage_safety_margin_multiplier);
        }

        Ok(())
    }
}
