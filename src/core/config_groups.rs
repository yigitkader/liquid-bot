/// Configuration groups for better organization
/// 
/// This module provides logical groupings of configuration fields
/// to improve code organization and maintainability.
/// 
/// The main `Config` struct still contains all fields for backward compatibility,
/// but these groups provide a cleaner way to access related settings.

use crate::core::config::Config;

/// RPC and network configuration
#[derive(Debug, Clone)]
pub struct RpcConfig {
    pub http_url: String,
    pub ws_url: String,
    pub poll_interval_ms: u64,
    pub timeout_seconds: u64,
}

impl From<&Config> for RpcConfig {
    fn from(config: &Config) -> Self {
        RpcConfig {
            http_url: config.rpc_http_url.clone(),
            ws_url: config.rpc_ws_url.clone(),
            poll_interval_ms: config.poll_interval_ms,
            timeout_seconds: 10, // Default timeout
        }
    }
}

/// Trading and liquidation parameters
#[derive(Debug, Clone)]
pub struct TradingConfig {
    pub min_profit_usd: f64,
    pub max_slippage_bps: u16,
    pub hf_liquidation_threshold: f64,
    pub liquidation_safety_margin: f64,
    pub liquidation_bonus: f64,
    pub close_factor: f64,
    pub max_liquidation_slippage: f64,
}

impl From<&Config> for TradingConfig {
    fn from(config: &Config) -> Self {
        TradingConfig {
            min_profit_usd: config.min_profit_usd,
            max_slippage_bps: config.max_slippage_bps,
            hf_liquidation_threshold: config.hf_liquidation_threshold,
            liquidation_safety_margin: config.liquidation_safety_margin,
            liquidation_bonus: config.liquidation_bonus,
            close_factor: config.close_factor,
            max_liquidation_slippage: config.max_liquidation_slippage,
        }
    }
}

/// Protocol-specific configuration (Solend, Pyth, Switchboard)
#[derive(Debug, Clone)]
pub struct ProtocolConfig {
    pub solend_program_id: String,
    pub pyth_program_id: String,
    pub switchboard_program_id: String,
    pub associated_token_program_id: String,
    pub main_lending_market_address: Option<String>,
    pub expected_reserve_size: usize,
}

impl From<&Config> for ProtocolConfig {
    fn from(config: &Config) -> Self {
        ProtocolConfig {
            solend_program_id: config.solend_program_id.clone(),
            pyth_program_id: config.pyth_program_id.clone(),
            switchboard_program_id: config.switchboard_program_id.clone(),
            associated_token_program_id: config.associated_token_program_id.clone(),
            main_lending_market_address: config.main_lending_market_address.clone(),
            expected_reserve_size: config.expected_reserve_size,
        }
    }
}

/// Oracle configuration
#[derive(Debug, Clone)]
pub struct OracleConfig {
    pub max_oracle_age_seconds: u64,
    pub oracle_read_fee_lamports: u64,
    pub oracle_accounts_read: u64,
    pub oracle_mappings_json: Option<String>,
    pub default_pyth_oracle_mappings_json: Option<String>,
    pub default_switchboard_oracle_mappings_json: Option<String>,
    pub default_oracle_confidence_slippage_bps: u16,
}

impl From<&Config> for OracleConfig {
    fn from(config: &Config) -> Self {
        OracleConfig {
            max_oracle_age_seconds: config.max_oracle_age_seconds,
            oracle_read_fee_lamports: config.oracle_read_fee_lamports,
            oracle_accounts_read: config.oracle_accounts_read,
            oracle_mappings_json: config.oracle_mappings_json.clone(),
            default_pyth_oracle_mappings_json: config.default_pyth_oracle_mappings_json.clone(),
            default_switchboard_oracle_mappings_json: config.default_switchboard_oracle_mappings_json.clone(),
            default_oracle_confidence_slippage_bps: config.default_oracle_confidence_slippage_bps,
        }
    }
}

/// Slippage estimation configuration
#[derive(Debug, Clone)]
pub struct SlippageConfig {
    pub use_jupiter_api: bool,
    pub slippage_final_multiplier: f64,
    pub slippage_size_small_threshold_usd: f64,
    pub slippage_size_large_threshold_usd: f64,
    pub slippage_multiplier_small: f64,
    pub slippage_multiplier_medium: f64,
    pub slippage_multiplier_large: f64,
    pub slippage_estimation_multiplier: f64,
    pub slippage_calibration_file: Option<String>,
    pub slippage_min_measurements_per_category: usize,
    pub z_score_95: f64,
}

impl From<&Config> for SlippageConfig {
    fn from(config: &Config) -> Self {
        SlippageConfig {
            use_jupiter_api: config.use_jupiter_api,
            slippage_final_multiplier: config.slippage_final_multiplier,
            slippage_size_small_threshold_usd: config.slippage_size_small_threshold_usd,
            slippage_size_large_threshold_usd: config.slippage_size_large_threshold_usd,
            slippage_multiplier_small: config.slippage_multiplier_small,
            slippage_multiplier_medium: config.slippage_multiplier_medium,
            slippage_multiplier_large: config.slippage_multiplier_large,
            slippage_estimation_multiplier: config.slippage_estimation_multiplier,
            slippage_calibration_file: config.slippage_calibration_file.clone(),
            slippage_min_measurements_per_category: config.slippage_min_measurements_per_category,
            z_score_95: config.z_score_95,
        }
    }
}

/// Transaction and execution configuration
#[derive(Debug, Clone)]
pub struct TransactionConfig {
    pub dry_run: bool,
    pub priority_fee_per_cu: u64,
    pub base_transaction_fee_lamports: u64,
    pub liquidation_compute_units: u32,
    pub default_compute_units: u32,
    pub default_priority_fee_per_cu: u64,
    pub tx_lock_timeout_seconds: u64,
    pub max_retries: u32,
    pub initial_retry_delay_ms: u64,
    pub retry_jitter_max_ms: u64,
    pub unwrap_wsol_after_liquidation: bool,
}

impl From<&Config> for TransactionConfig {
    fn from(config: &Config) -> Self {
        TransactionConfig {
            dry_run: config.dry_run,
            priority_fee_per_cu: config.priority_fee_per_cu,
            base_transaction_fee_lamports: config.base_transaction_fee_lamports,
            liquidation_compute_units: config.liquidation_compute_units,
            default_compute_units: config.default_compute_units,
            default_priority_fee_per_cu: config.default_priority_fee_per_cu,
            tx_lock_timeout_seconds: config.tx_lock_timeout_seconds,
            max_retries: config.max_retries,
            initial_retry_delay_ms: config.initial_retry_delay_ms,
            retry_jitter_max_ms: config.retry_jitter_max_ms,
            unwrap_wsol_after_liquidation: config.unwrap_wsol_after_liquidation,
        }
    }
}

/// System and performance configuration
#[derive(Debug, Clone)]
pub struct SystemConfig {
    pub wallet_path: String,
    pub min_reserve_lamports: u64,
    pub event_bus_buffer_size: usize,
    pub analyzer_max_workers: usize,
    pub analyzer_max_workers_limit: usize,
    pub health_manager_max_error_age_seconds: u64,
    pub max_consecutive_errors: u32,
    pub ws_listener_sleep_seconds: u64,
}

impl From<&Config> for SystemConfig {
    fn from(config: &Config) -> Self {
        SystemConfig {
            wallet_path: config.wallet_path.clone(),
            min_reserve_lamports: config.min_reserve_lamports,
            event_bus_buffer_size: config.event_bus_buffer_size,
            analyzer_max_workers: config.analyzer_max_workers,
            analyzer_max_workers_limit: config.analyzer_max_workers_limit,
            health_manager_max_error_age_seconds: config.health_manager_max_error_age_seconds,
            max_consecutive_errors: config.max_consecutive_errors,
            ws_listener_sleep_seconds: config.ws_listener_sleep_seconds,
        }
    }
}

/// Token mint addresses configuration
#[derive(Debug, Clone)]
pub struct TokenConfig {
    pub usdc_mint: String,
    pub sol_mint: String,
    pub usdt_mint: Option<String>,
    pub eth_mint: Option<String>,
    pub btc_mint: Option<String>,
    pub usdc_reserve_address: Option<String>,
    pub sol_reserve_address: Option<String>,
    pub sol_price_fallback_usd: f64,
}

impl From<&Config> for TokenConfig {
    fn from(config: &Config) -> Self {
        TokenConfig {
            usdc_mint: config.usdc_mint.clone(),
            sol_mint: config.sol_mint.clone(),
            usdt_mint: config.usdt_mint.clone(),
            eth_mint: config.eth_mint.clone(),
            btc_mint: config.btc_mint.clone(),
            usdc_reserve_address: config.usdc_reserve_address.clone(),
            sol_reserve_address: config.sol_reserve_address.clone(),
            sol_price_fallback_usd: config.sol_price_fallback_usd,
        }
    }
}

/// Profit calculation configuration
#[derive(Debug, Clone)]
pub struct ProfitConfig {
    pub min_profit_margin_bps: u16,
    pub dex_fee_bps: u16,
}

impl From<&Config> for ProfitConfig {
    fn from(config: &Config) -> Self {
        ProfitConfig {
            min_profit_margin_bps: config.min_profit_margin_bps,
            dex_fee_bps: config.dex_fee_bps,
        }
    }
}

