use std::env;
use anyhow::Result;

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
        Ok(Config {
            rpc_http_url: env::var("RPC_HTTP_URL")
                .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string()),
            rpc_ws_url: env::var("RPC_WS_URL")
                .unwrap_or_else(|_| "wss://api.mainnet-beta.solana.com".to_string()),
            wallet_path: env::var("WALLET_PATH")
                .unwrap_or_else(|_| "./wallet.json".to_string()),
            hf_liquidation_threshold: env::var("HF_LIQUIDATION_THRESHOLD")
                .unwrap_or_else(|_| "1.0".to_string())
                .parse()
                .unwrap_or(1.0),
            min_profit_usd: env::var("MIN_PROFIT_USD")
                .unwrap_or_else(|_| "1.0".to_string())
                .parse()
                .unwrap_or(1.0),
            max_slippage_bps: env::var("MAX_SLIPPAGE_BPS")
                .unwrap_or_else(|_| "50".to_string())
                .parse()
                .unwrap_or(50),
            poll_interval_ms: env::var("POLL_INTERVAL_MS")
                .unwrap_or_else(|_| "2000".to_string())
                .parse()
                .unwrap_or(2000),
            dry_run: env::var("DRY_RUN")
                .unwrap_or_else(|_| "true".to_string())
                .parse()
                .unwrap_or(true),
        })
    }
}

