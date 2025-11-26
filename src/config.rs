use std::env;
use std::path::Path;
use anyhow::{Context, Result};

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
                .unwrap_or_else(|_| "1.0".to_string())
                .parse()
                .context("Invalid MIN_PROFIT_USD value")?,
            max_slippage_bps: env::var("MAX_SLIPPAGE_BPS")
                .unwrap_or_else(|_| "50".to_string())
                .parse()
                .context("Invalid MAX_SLIPPAGE_BPS value")?,
            poll_interval_ms: env::var("POLL_INTERVAL_MS")
                .unwrap_or_else(|_| "2000".to_string())
                .parse()
                .context("Invalid POLL_INTERVAL_MS value")?,
            dry_run: env::var("DRY_RUN")
                .unwrap_or_else(|_| "true".to_string())
                .parse()
                .context("Invalid DRY_RUN value (must be 'true' or 'false')")?,
        };

        // Configuration validation
        config.validate()?;
        
        Ok(config)
    }

    /// Configuration validation - production için kritik
    pub fn validate(&self) -> Result<()> {
        // RPC URL validation
        if !self.rpc_http_url.starts_with("http://") && !self.rpc_http_url.starts_with("https://") {
            return Err(anyhow::anyhow!("RPC_HTTP_URL must start with http:// or https://"));
        }

        if !self.rpc_ws_url.starts_with("ws://") && !self.rpc_ws_url.starts_with("wss://") {
            return Err(anyhow::anyhow!("RPC_WS_URL must start with ws:// or wss://"));
        }

        // Wallet path validation
        if !Path::new(&self.wallet_path).exists() {
            return Err(anyhow::anyhow!(
                "Wallet file not found: {}. Please create wallet first.",
                self.wallet_path
            ));
        }

        // Threshold validation
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

        if self.max_slippage_bps > 10000 {
            return Err(anyhow::anyhow!(
                "MAX_SLIPPAGE_BPS must be <= 10000 (100%), got: {}",
                self.max_slippage_bps
            ));
        }

        // Poll interval validation
        if self.poll_interval_ms < 100 {
            return Err(anyhow::anyhow!(
                "POLL_INTERVAL_MS must be >= 100ms, got: {}",
                self.poll_interval_ms
            ));
        }

        // Production safety check
        if !self.dry_run {
            log::warn!("⚠️  DRY_RUN=false: Bot will send REAL transactions to blockchain!");
            log::warn!("⚠️  Make sure you have tested thoroughly in dry-run mode!");
        }

        Ok(())
    }
}

