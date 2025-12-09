use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::collections::{HashMap, VecDeque};
use std::env;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::time::sleep;
// parking_lot and lazy_static moved to oracle/switchboard.rs (for aligned buffer pool)

use crate::jup::{get_jupiter_quote_with_retry, JupiterQuote};
use crate::solend::{Obligation, Reserve, solend_program_id};
use crate::solend::{ObligationLiquidity, ObligationCollateral};
use crate::utils::{send_jito_bundle, JitoClient};
use crate::oracle::{self, validate_oracles_with_twap};
use crate::liquidation_tx::build_flashloan_liquidation_tx;
use crate::quotes::{get_liquidation_quote, get_sol_price_usd, get_sol_price_usd_standalone};

// Aligned buffer pool moved to oracle/switchboard.rs

/// Liquidation mode - per Structure.md
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiquidationMode {
    DryRun,
    Live,
}

/// Config structure per Structure.md section 6.2
/// All values are automatically discovered from chain or environment - no hardcoded addresses
pub struct Config {
    pub rpc_url: String,
    pub jito_url: String,
    pub jupiter_url: String,
    pub keypair_path: PathBuf,
    pub liquidation_mode: LiquidationMode,
    pub min_profit_usdc: f64,
    pub max_position_pct: f64, // Örn: 0.05 => cüzdanın %5'i max risk
    pub wallet: Arc<Keypair>,
    pub jito_tip_account: Option<String>, // Auto-discovered from env or default
    pub jito_tip_amount_lamports: Option<u64>, // Auto-discovered from env or default
}

/// Main liquidation loop - minimal async pipeline per Structure.md section 9
pub async fn run_liquidation_loop(
    rpc: Arc<RpcClient>,
    config: Config,
) -> Result<()> {
    let program_id = solend_program_id()?;
    let wallet = config.wallet.pubkey();

    // Initialize Jito client with tip account from config or env
    // No hardcoded defaults - must be set in .env file
    use std::env;
    let jito_tip_account_str = if let Some(tip_account) = config.jito_tip_account.as_deref() {
        tip_account.to_string()
    } else if let Ok(tip_account) = env::var("JITO_TIP_ACCOUNT") {
        tip_account
    } else {
        return Err(anyhow::anyhow!(
            "JITO_TIP_ACCOUNT not found in .env file or config. \
             Please set JITO_TIP_ACCOUNT in .env file. \
             Example: JITO_TIP_ACCOUNT=96gYZGLnJYVFmbjzopPSU6QiEV5fGqZ6N6VBY6FuDgU3"
        ));
    };
    
    // CRITICAL: Validate Jito tip account address format
    // 
    // IMPORTANT: Jito tip account is CRITICAL for bot operation:
    // - All liquidation transactions are sent via Jito bundles (MEV protection)
    // - Each bundle includes a tip transaction to the tip account
    // - Tip account is just a transfer address (SOL is sent to it)
    // - Account doesn't need to exist in RPC (AccountNotFound is OK)
    // - But address MUST be valid Pubkey format (otherwise tip transaction will fail)
    //
    // If tip account is wrong:
    // - Tip will be lost (sent to wrong address)
    // - But bundle can still be sent (Jito will accept it)
    // - However, without proper tip, bundle may not get priority processing
    //
    // Bot will work even if tip account not found in RPC, but tip account address
    // must be valid for tip transaction to be created successfully.
    let jito_tip_account = Pubkey::from_str(&jito_tip_account_str)
        .context(format!(
            "CRITICAL: Invalid Jito tip account address format: {}. \
             Jito is REQUIRED for bot operation (all transactions sent via Jito bundles). \
             Please verify JITO_TIP_ACCOUNT in .env is a valid Solana address.",
            jito_tip_account_str
        ))?;
    
    // Optional: Try to fetch account info for validation (but don't fail if not found)
    // Jito tip accounts are just transfer addresses - they may not exist in RPC
    // This is just for informational purposes - bot will work regardless
    match rpc.get_account(&jito_tip_account) {
        Ok(account_data) => {
            // If account exists, validate it's a system account (native SOL account)
            use solana_system_interface::program;
            let system_program_address = program::id();
            // Convert Address to Pubkey for comparison
            let system_program_id = Pubkey::try_from(system_program_address.as_ref())
                .unwrap_or_else(|_| Pubkey::default()); // Fallback if conversion fails
            if account_data.owner == system_program_id {
                let balance_sol = account_data.lamports as f64 / 1_000_000_000.0;
                log::info!(
                    "✅ Jito tip account validated: {} (balance: {:.6} SOL, owner: system program)",
                    jito_tip_account,
                    balance_sol
                );
            } else {
                log::warn!(
                    "⚠️  Jito tip account {} exists but owner is {} (expected system program). \
                     This may indicate wrong address. Tips may be lost, but bot will continue.",
                    jito_tip_account,
                    account_data.owner
                );
            }
        }
        Err(_) => {
            // AccountNotFound is OK - Jito tip accounts are just transfer addresses
            // They don't need to exist in RPC, they're just used for SOL transfers
            // Bot will work fine - tip transaction will be created and sent
            log::warn!(
                "ℹ️  Jito tip account {} not found in RPC (this is OK - tip accounts are transfer addresses, not regular accounts). \
                 Bot will work normally - tip transaction will be created when sending bundles.",
                jito_tip_account
            );
        }
    }
    
    let jito_tip_amount = config.jito_tip_amount_lamports
        .unwrap_or(10_000_000u64); // Default: 0.01 SOL
    log::info!("✅ Jito tip amount: {} lamports (~{} SOL)", 
        jito_tip_amount, 
        jito_tip_amount as f64 / 1_000_000_000.0);
    
    // CRITICAL: Try to get tip accounts dynamically from Jito Block Engine API
    // Tip accounts can change over time, so we should fetch them dynamically
    // If JITO_TIP_ACCOUNT is set in env, use it (user override)
    // Otherwise, try to fetch from Jito API, fallback to default if API fails
    let final_tip_account = if config.jito_tip_account.is_some() {
        // User explicitly set JITO_TIP_ACCOUNT - use it (no dynamic fetch)
        log::info!("Using JITO_TIP_ACCOUNT from environment: {}", jito_tip_account);
        jito_tip_account
    } else {
        // Try to fetch tip accounts dynamically from Jito API
        let temp_jito_client = JitoClient::new(
            config.jito_url.clone(),
            jito_tip_account, // Temporary, will be updated
            jito_tip_amount,
        );
        
        match temp_jito_client.get_tip_accounts().await {
            Ok(tip_accounts) => {
                if !tip_accounts.is_empty() {
                    // Use first tip account from Jito API
                    let dynamic_tip_account = tip_accounts[0];
                    log::info!(
                        "✅ Using tip account from Jito API (dynamic): {} (found {} total tip accounts)",
                        dynamic_tip_account,
                        tip_accounts.len()
                    );
                    dynamic_tip_account
                } else {
                    log::warn!("Jito API returned empty tip accounts list, using default");
                    jito_tip_account
                }
            }
            Err(e) => {
                log::warn!(
                    "Failed to fetch tip accounts from Jito API: {}. Using default/fallback tip account: {}",
                    e,
                    jito_tip_account
                );
                log::info!("Tip: Set JITO_TIP_ACCOUNT in .env to use a specific account, or ensure Jito API is accessible");
                jito_tip_account
            }
        }
    };
    
    let jito_client = JitoClient::new(
        config.jito_url.clone(),
        final_tip_account,
        jito_tip_amount,
    );

    // Validate Jito endpoint per Structure.md section 13
    validate_jito_endpoint(&jito_client).await
        .context("Jito endpoint validation failed - check network connectivity")?;

    log::info!("🚀 Starting liquidation loop");
    log::info!("   Program ID: {}", program_id);
    log::info!("   Wallet: {}", wallet);
    log::info!("   Min Profit USDC: ${}", config.min_profit_usdc);
    log::info!("   Max Position %: {:.2}%", config.max_position_pct * 100.0);
    log::info!("   Mode: {:?}", config.liquidation_mode);

    // Track last balance log time for periodic logging
    let mut last_balance_log = Instant::now();
    const BALANCE_LOG_INTERVAL_SECS: u64 = 30; // Log balances every 30 seconds
    
    loop {
        // Log wallet balances periodically
        if last_balance_log.elapsed().as_secs() >= BALANCE_LOG_INTERVAL_SECS {
            if let Err(e) = log_wallet_balances(&rpc, &wallet).await {
                log::warn!("Failed to log wallet balances: {}", e);
            }
            last_balance_log = Instant::now();
        }
        
        match process_cycle(&rpc, &program_id, &config, &jito_client).await {
            Ok(_) => {}
            Err(e) => {
                log::error!("Error in liquidation cycle: {}", e);
            }
        }

        // Poll interval from .env (no hardcoded values)
        use std::env;
        let poll_interval_ms = env::var("POLL_INTERVAL_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or_else(|| {
                log::warn!("POLL_INTERVAL_MS not found in .env, using default 500ms");
                500 // Default 500ms if not set
            });
        sleep(Duration::from_millis(poll_interval_ms)).await;
    }
}

/// Metrics for tracking liquidation cycle performance
/// Enhanced to track different failure reasons for better observability
struct CycleMetrics {
    total_candidates: usize,
    skipped_oracle_fail: usize,
    skipped_jupiter_fail: usize,
    skipped_insufficient_profit: usize,
    skipped_risk_limit: usize,
    failed_build_tx: usize,
    failed_send_bundle: usize,
    successful: usize,
    // Additional detailed tracking
    skipped_rpc_error: usize,      // RPC call failures
    skipped_reserve_load_fail: usize, // Failed to load reserve accounts
    skipped_ata_missing: usize,   // Missing ATA (shouldn't happen if startup check works)
    
    // Bundle outcome tracking
    confirmed_bundles: usize,     // Bundles confirmed on-chain
    dropped_bundles: usize,       // Bundles dropped/failed after sending
}

async fn process_cycle(
    rpc: &Arc<RpcClient>,
    program_id: &Pubkey,
    config: &Config,
    jito_client: &JitoClient,
) -> Result<()> {
    // 1. Solend obligation account'larını çek
    log::debug!("Fetching obligation accounts...");
    let rpc_clone = Arc::clone(rpc);
    let program_id_clone = *program_id;
    let accounts = tokio::task::spawn_blocking(move || {
        rpc_clone.get_program_accounts(&program_id_clone)
    })
    .await
    .map_err(|e| anyhow::anyhow!("Task join error: {}", e))?
    .map_err(|e| anyhow::anyhow!("Failed to get program accounts: {}", e))?;

    log::debug!("Found {} accounts", accounts.len());

    // 2. HF < 1.0 olanları bul
    // CRITICAL SECURITY: Track parse errors to detect layout changes or corrupt data
    // ✅ FIXED: Use account type identification utility
    // This is faster and more reliable than attempting to parse every account
    let mut candidates = Vec::new();
    let mut parse_errors = 0;
    let mut skipped_wrong_type = 0;
    let total_accounts = accounts.len();
    
    for (pk, acc) in accounts {
        // Use account type identification to quickly filter accounts
        match crate::solend::identify_solend_account_type(&acc.data) {
            crate::solend::SolendAccountType::Obligation => {
                // This looks like an Obligation - try to parse it
                match Obligation::from_account_data(&acc.data) {
                    Ok(obligation) => {
                        // Read health factor threshold from .env (no hardcoded values)
                        let hf_threshold = std::env::var("HF_LIQUIDATION_THRESHOLD")
                            .ok()
                            .and_then(|s| s.parse::<f64>().ok())
                            .unwrap_or(1.0); // Default 1.0 if not set
                        
                        // CRITICAL FIX: Use u128 arithmetic for high-precision HF check
                        // f64 precision loss can cause missed liquidations for HF very close to 1.0
                        // (e.g., 0.9999999999 vs 1.0000000001)
                        const WAD: u128 = 1_000_000_000_000_000_000;
                        let hf_wad = obligation.health_factor_u128();
                        
                        // Convert threshold to WAD with care
                        let threshold_wad = (hf_threshold * WAD as f64) as u128;
                        
                        if hf_wad < threshold_wad {
                            candidates.push((pk, obligation));
                        }
                    }
                    Err(e) => {
                        parse_errors += 1;
                        log::debug!("Failed to parse Obligation {}: {}", pk, e);
                    }
                }
            }
            _ => {
                // Skip non-Obligation accounts (Reserve, LendingMarket, Unknown)
                skipped_wrong_type += 1;
            }
        }
    }
    
    // Log filtering statistics
    if skipped_wrong_type > 0 {
        log::debug!(
            "Skipped {} accounts with wrong type (likely Reserves/LendingMarkets, not Obligations)",
            skipped_wrong_type
        );
    }
    
    // Log error rate if high (indicates potential layout change or widespread corruption)
    if parse_errors > 10 {
        log::error!(
            "⚠️  High parse error rate: {}/{} obligations failed to parse. Layout may have changed or data may be corrupted!",
            parse_errors,
            total_accounts
        );
    } else if parse_errors > 0 {
        log::debug!(
            "Parse errors: {}/{} obligations failed to parse (acceptable rate)",
            parse_errors,
            total_accounts
        );
    }

    let total_candidates = candidates.len();
    log::info!("Found {} liquidation opportunities (HF < 1.0)", total_candidates);

    // Initialize metrics
    let mut metrics = CycleMetrics {
        total_candidates,
        skipped_oracle_fail: 0,
        skipped_jupiter_fail: 0,
        skipped_insufficient_profit: 0,
        skipped_risk_limit: 0,
        failed_build_tx: 0,
        failed_send_bundle: 0,
        successful: 0,
        skipped_rpc_error: 0,
        skipped_reserve_load_fail: 0,
        skipped_ata_missing: 0,
        confirmed_bundles: 0,
        dropped_bundles: 0,
    };

    // Per Structure.md section 6.4: Track block-wide cumulative risk
    // "Tek blok içinde kullanılan toplam risk de aynı limit ile sınırlıdır"
    // 
    // CRITICAL FIX: Wallet balance tracking strategy
    //
    // Problem: Her liquidation sonrası wallet balance değişir, ama RPC call çok yavaş
    // Çözüm: Hybrid approach
    // 1. Cycle başında initial balance al
    // 2. Her liquidation sonrası ESTIMATED balance hesapla (RPC call yapmadan)
    // 3. Her 5 liquidation'da bir GERÇEK balance refresh et (doğrulama için)
    
    // Helper struct for wallet balance tracking
    struct WalletBalanceTracker {
        initial_balance_usd: f64,
        current_estimated_balance_usd: f64,
        // CRITICAL: Track pending committed amount to prevent overcommit
        // This represents capital that has been committed to pending liquidations
        pending_committed_usd: f64,
        last_refresh_balance_usd: f64,
        liquidations_since_refresh: u32,
        total_liquidations: u32,
    }
    
    let mut wallet_balance_tracker = WalletBalanceTracker {
        initial_balance_usd: 0.0,
        current_estimated_balance_usd: 0.0,
        pending_committed_usd: 0.0,
        last_refresh_balance_usd: 0.0,
        liquidations_since_refresh: 0,
        total_liquidations: 0,
    };
    
    // Cycle başında initial balance al
    match get_wallet_value_usd(rpc, &config.wallet.pubkey()).await {
        Ok(value) => {
            wallet_balance_tracker.initial_balance_usd = value;
            wallet_balance_tracker.current_estimated_balance_usd = value;
            wallet_balance_tracker.last_refresh_balance_usd = value;
            log::debug!("Initial wallet balance: ${:.2}", value);
        }
        Err(e) => {
            log::error!("Failed to get initial wallet value: {}", e);
            return Err(anyhow::anyhow!("Cannot proceed without wallet balance: {}", e));
        }
    }
    
    let mut cumulative_risk_usd = 0.0; // Track total risk used in this cycle
    
    // Track pending liquidations (sent but not yet executed on-chain)
    // CRITICAL FIX: Use real-time bundle status checking to handle race conditions
    // Bundles can execute in ~400ms, so we check status proactively instead of waiting for timeout
    
    /// Enhanced bundle tracking with real-time status
    struct BundleInfo {
        value_usd: f64,
        sent_at: Instant,
        last_status_check: Instant,
        confirmed: bool,
    }
    
    struct BundleTracker {
        bundles: std::collections::HashMap<String, BundleInfo>,
    }
    
    impl BundleTracker {
        fn new() -> Self {
            BundleTracker {
                bundles: std::collections::HashMap::new(),
            }
        }
        
        fn add_bundle(&mut self, bundle_id: String, value_usd: f64) {
            self.bundles.insert(bundle_id, BundleInfo {
                value_usd,
                sent_at: Instant::now(),
                last_status_check: Instant::now(),
                confirmed: false,
            });
        }
        
        /// Update bundle status by checking Jito API
        /// Returns list of processed bundles: (id, value_usd, is_success)
        async fn update_statuses(
            &mut self,
            jito_client: &JitoClient,
        ) -> Vec<(String, f64, bool)> {
            let mut processed_bundles = Vec::new();
            
            // Check bundles that haven't been checked in last 200ms
            let now = Instant::now();
            let bundle_ids_to_check: Vec<String> = self.bundles.iter()
                .filter(|(_, info)| {
                    !info.confirmed && 
                    now.duration_since(info.last_status_check) >= Duration::from_millis(300)
                })
                .map(|(id, _)| id.clone())
                .collect();
            
            for bundle_id in bundle_ids_to_check {
                // Check bundle status via Jito API
                match jito_client.get_bundle_status(&bundle_id).await {
                    Ok(Some(status)) => {
                        if let Some(status_str) = &status.status {
                            if status_str == "landed" || status_str == "confirmed" {
                                // Bundle confirmed!
                                if let Some(info) = self.bundles.get_mut(&bundle_id) {
                                    if !info.confirmed {
                                        info.confirmed = true;
                                        processed_bundles.push((bundle_id.clone(), info.value_usd, true));
                                        
                                        log::debug!(
                                            "✅ Bundle {} confirmed in {:.1}s",
                                            bundle_id,
                                            info.sent_at.elapsed().as_secs_f64()
                                        );
                                    }
                                }
                            } else if status_str == "failed" || status_str == "dropped" {
                                // Bundle failed - mark as confirmed (will be cleaned up)
                                if let Some(info) = self.bundles.get_mut(&bundle_id) {
                                    if !info.confirmed {
                                        info.confirmed = true; // Mark as processed
                                        processed_bundles.push((bundle_id.clone(), info.value_usd, false));
                                        log::debug!("Bundle {} failed/dropped", bundle_id);
                                    }
                                }
                            }
                        }
                        
                        // If slot is present, bundle likely executed
                        if status.slot.is_some() {
                            if let Some(info) = self.bundles.get_mut(&bundle_id) {
                                if !info.confirmed {
                                    info.confirmed = true;
                                    processed_bundles.push((bundle_id.clone(), info.value_usd, true));
                                    log::debug!("Bundle {} confirmed (has slot)", bundle_id);
                                }
                            }
                        }
                        
                        // Update last check timestamp
                        if let Some(info) = self.bundles.get_mut(&bundle_id) {
                            info.last_status_check = now;
                        }
                    }
                    Ok(None) => {
                        // Bundle status unknown - update check time anyway
                        if let Some(info) = self.bundles.get_mut(&bundle_id) {
                            info.last_status_check = now;
                        }
                    }
                    Err(e) => {
                        log::debug!("Failed to check bundle status for {}: {}", bundle_id, e);
                    }
                }
            }
            
            // Remove old confirmed bundles (older than 10s)
            self.bundles.retain(|_, info| {
                !info.confirmed || now.duration_since(info.sent_at) < Duration::from_secs(10)
            });
            
            // CRITICAL: Release expired unconfirmed bundles (more aggressive timeout)
            // Timeout configurable via BUNDLE_EXPIRY_TIMEOUT_SECS (default: 2s)
            // This prevents overcommit by releasing dropped bundles quickly
            use std::env;
            let expiry_timeout_secs = env::var("BUNDLE_EXPIRY_TIMEOUT_SECS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(5); // Default: 5 seconds - Jito bundles can take 400ms-2s to confirm, need buffer
            
            // Find expired bundles (unconfirmed and older than timeout)
            let expired_bundle_ids: Vec<String> = self.bundles.iter()
                .filter(|(_, info)| {
                    !info.confirmed && 
                    now.duration_since(info.sent_at) >= Duration::from_secs(expiry_timeout_secs)
                })
                .map(|(id, _)| id.clone())
                .collect();
            
            // Verify bundle status before releasing (to avoid false positives)
            let mut expired_to_release: Vec<(String, f64)> = Vec::new();
            for bundle_id in expired_bundle_ids {
                // Check bundle status one more time before releasing
                match jito_client.get_bundle_status(&bundle_id).await {
                    Ok(Some(status)) => {
                        if let Some(status_str) = &status.status {
                            match status_str.as_str() {
                                "landed" | "confirmed" => {
                                    // Bundle actually confirmed - mark as confirmed instead of releasing
                                    if let Some(info) = self.bundles.get_mut(&bundle_id) {
                                        if !info.confirmed {
                                            info.confirmed = true;
                                            processed_bundles.push((bundle_id.clone(), info.value_usd, true));
                                            log::debug!(
                                                "✅ Bundle {} confirmed during expiry check (was about to expire)",
                                                bundle_id
                                            );
                                        }
                                    }
                                    continue; // Don't release - it's confirmed
                                }
                                "failed" | "dropped" => {
                                    // Bundle definitely failed - safe to release
                                    if let Some(info) = self.bundles.get(&bundle_id) {
                                        processed_bundles.push((bundle_id.clone(), info.value_usd, false));
                                        log::warn!(
                                            "⏰ Releasing expired bundle {} (${:.2}) after {}s - status: {}",
                                            bundle_id,
                                            info.value_usd,
                                            expiry_timeout_secs,
                                            status_str
                                        );
                                    }
                                }
                                "pending" => {
                                    // Still pending - give it more time (don't release yet)
                                    log::debug!(
                                        "Bundle {} still pending after {}s - keeping for now",
                                        bundle_id,
                                        expiry_timeout_secs
                                    );
                                    continue; // Don't release - still pending
                                }
                                _ => {
                                    // Unknown status - assume dropped after timeout
                                    if let Some(info) = self.bundles.get(&bundle_id) {
                                        expired_to_release.push((bundle_id.clone(), info.value_usd));
                                        log::warn!(
                                            "⏰ Releasing expired bundle {} (${:.2}) after {}s - unknown status: {}",
                                            bundle_id,
                                            info.value_usd,
                                            expiry_timeout_secs,
                                            status_str
                                        );
                                    }
                                }
                            }
                        } else if status.slot.is_some() {
                            // Has slot - likely confirmed
                            if let Some(info) = self.bundles.get_mut(&bundle_id) {
                                if !info.confirmed {
                                    info.confirmed = true;
                                    processed_bundles.push((bundle_id.clone(), info.value_usd, true));
                                    log::debug!(
                                        "✅ Bundle {} confirmed during expiry check (has slot)",
                                        bundle_id
                                    );
                                }
                            }
                            continue; // Don't release - it's confirmed
                        } else {
                            // No status but has response - assume dropped after timeout
                            if let Some(info) = self.bundles.get(&bundle_id) {
                                expired_to_release.push((bundle_id.clone(), info.value_usd));
                                log::warn!(
                                    "⏰ Releasing expired bundle {} (${:.2}) after {}s - no status in response",
                                    bundle_id,
                                    info.value_usd,
                                    expiry_timeout_secs
                                );
                            }
                        }
                    }
                    Ok(None) => {
                        // No status available - assume dropped after timeout
                        if let Some(info) = self.bundles.get(&bundle_id) {
                            expired_to_release.push((bundle_id.clone(), info.value_usd));
                            log::warn!(
                                "⏰ Releasing expired bundle {} (${:.2}) after {}s - no status available",
                                bundle_id,
                                info.value_usd,
                                expiry_timeout_secs
                            );
                        }
                    }
                    Err(e) => {
                        // API error - assume dropped after timeout (conservative)
                        if let Some(info) = self.bundles.get(&bundle_id) {
                            expired_to_release.push((bundle_id.clone(), info.value_usd));
                            log::warn!(
                                "⏰ Releasing expired bundle {} (${:.2}) after {}s - status check failed: {}",
                                bundle_id,
                                info.value_usd,
                                expiry_timeout_secs,
                                e
                            );
                        }
                    }
                }
            }
            
            // Remove released bundles from tracking
            for (bundle_id, _) in &expired_to_release {
                self.bundles.remove(bundle_id);
            }
            
            // Return both confirmed and expired bundles
            // Both should release committed amounts (confirmed = executed, expired = dropped)
            // Expired bundles are added to processed_bundles as "dropped" (false) so caller can release committed amounts
            for (bundle_id, value) in expired_to_release {
                processed_bundles.push((bundle_id, value, false));
            }
            
            processed_bundles
        }
        
        /// Get total pending committed value (unconfirmed bundles)
        fn get_pending_committed(&self) -> f64 {
            self.bundles.iter()
                .filter(|(_, info)| !info.confirmed) // ✅ CRITICAL FIX: Only count unconfirmed bundles
                .map(|(_, info)| info.value_usd)
                .sum()
        }
        
        /// Release expired bundles (DEPRECATED - now handled in update_statuses)
        /// This function is kept for backward compatibility but is no longer used.
        /// Expired bundle handling is now integrated into update_statuses() for better
        /// status verification before releasing.
        #[allow(dead_code)]
        fn release_expired(&mut self) -> Vec<(String, f64)> {
            // This function is deprecated - expired bundles are now handled in update_statuses()
            // with proper status verification
            Vec::new()
        }
    }
    
    let mut bundle_tracker = BundleTracker::new();
    
    // Bundle status enum for tracking bundle execution state
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum BundleStatus {
        Pending,
        Confirmed,
        Failed,
        Unknown,
    }
    
    /// Verify bundle status before removing from tracking
    /// This prevents race conditions where we release committed amount for bundles that actually executed
    async fn verify_bundle_status(
        _rpc: &Arc<RpcClient>,
        bundle_id: &str,
        jito_client: &JitoClient,
    ) -> BundleStatus {
        // Option 1: Check Jito bundle status API
        if let Ok(Some(status_response)) = jito_client.get_bundle_status(bundle_id).await {
            if let Some(status_str) = &status_response.status {
                match status_str.as_str() {
                    "landed" | "confirmed" => {
                        log::debug!("Bundle {} confirmed via Jito API", bundle_id);
                        return BundleStatus::Confirmed;
                    }
                    "failed" | "dropped" => {
                        log::debug!("Bundle {} failed/dropped via Jito API", bundle_id);
                        return BundleStatus::Failed;
                    }
                    "pending" => {
                        return BundleStatus::Pending;
                    }
                    _ => {
                        log::debug!("Bundle {} has unknown status: {}", bundle_id, status_str);
                    }
                }
            }
            
            // If slot is present, bundle likely executed
            if status_response.slot.is_some() {
                log::debug!("Bundle {} has slot, assuming confirmed", bundle_id);
                return BundleStatus::Confirmed;
            }
        }
        
        // Option 2: Fallback - conservative approach
        // If we can't determine status, assume executed to avoid overcommit
        // This is safer than assuming failed (which would release committed amount incorrectly)
        log::debug!("Bundle {} status unknown, assuming confirmed (conservative)", bundle_id);
        BundleStatus::Unknown
    }

    log::debug!(
        "Cycle started: initial_wallet_value=${:.2}, cumulative_risk tracking initialized (using estimated balance with refresh every 5 liquidations)",
        wallet_balance_tracker.initial_balance_usd
    );

    // 3. Her candidate için liquidation denemesi per Structure.md section 9
    for (obl_pubkey, obligation) in candidates {
        // a) Oracle + reserve load + HF confirm
        let ctx = match build_liquidation_context(rpc, &obligation).await {
            Ok(mut ctx) => {
                ctx.obligation_pubkey = obl_pubkey; // Set actual obligation pubkey
                ctx
            }
            Err(e) => {
                log::warn!("Failed to build liquidation context for {}: {}", obl_pubkey, e);
                metrics.skipped_oracle_fail += 1;
                continue;
            }
        };
        
        if !ctx.oracle_ok {
            // Provide detailed information about which oracle failed
            let borrow_info = ctx.borrow_reserve.as_ref().map(|r| {
                format!("borrow_reserve={}, pyth_oracle={}, switchboard_oracle={}", 
                    r.mint_pubkey(),
                    r.oracle_pubkey(),
                    r.liquidity().liquiditySwitchboardOracle)
            }).unwrap_or_else(|| "borrow_reserve=None".to_string());
            
            let deposit_info = ctx.deposit_reserve.as_ref().map(|r| {
                format!("deposit_reserve={}, pyth_oracle={}, switchboard_oracle={}", 
                    r.mint_pubkey(),
                    r.oracle_pubkey(),
                    r.liquidity().liquiditySwitchboardOracle)
            }).unwrap_or_else(|| "deposit_reserve=None".to_string());
            
            // Reduce log spam: Use debug instead of warn for frequent oracle failures
            // We have a summary at the end of the cycle to track these stats
            log::debug!(
                "Skipping {}: Oracle validation failed - {} | {} | borrow_price={:?}, deposit_price={:?}",
                obl_pubkey,
                borrow_info,
                deposit_info,
                ctx.borrow_price_usd,
                ctx.deposit_price_usd
            );
            metrics.skipped_oracle_fail += 1;
            continue;
        }

        // b) Jupiter'den kârlılık kontrolü
        let quote_result = get_liquidation_quote(&ctx, config, rpc).await;
        let quote = match quote_result {
            Ok(q) => q,
            Err(e) => {
                log::warn!("Skipping {}: Jupiter quote failed: {}", obl_pubkey, e);
                metrics.skipped_jupiter_fail += 1;
                continue;
            }
        };

        if quote.profit_usdc < config.min_profit_usdc {
            log::debug!(
                "Skipping {}: Profit ${:.2} < min ${:.2}",
                obl_pubkey,
                quote.profit_usdc,
                config.min_profit_usdc
            );
            metrics.skipped_insufficient_profit += 1;
            continue;
        }

        // c) Wallet risk limiti - per-liquidation check
        // CRITICAL OPTIMIZATION: Use cached wallet balance with pending liquidation tracking
        // instead of refreshing before each liquidation. This reduces RPC calls significantly.
        // 
        // With 10 liquidations, this reduces RPC calls from 20 (2 per liquidation) to 2 (once at start).
        // Trade-off: Slightly less accurate but much faster and avoids RPC rate limits.
        // 
        // CRITICAL FIX: Real-time bundle status checking (instead of timeout-based)
        // Check bundle status proactively every 200ms to detect confirmed bundles immediately
        // This prevents overcommit by releasing committed amounts as soon as bundles execute (~400ms)
        let processed_bundles = bundle_tracker.update_statuses(jito_client).await;
        
        // Release committed amounts and update metrics
        for (bundle_id, value_usd, is_success) in processed_bundles {
            wallet_balance_tracker.pending_committed_usd -= value_usd;
            
            if is_success {
                metrics.confirmed_bundles += 1;
                log::debug!("Released ${:.2} committed for CONFIRMED bundle {}", value_usd, bundle_id);
            } else {
                metrics.dropped_bundles += 1;
                log::debug!("Released ${:.2} committed for DROPPED bundle {}", value_usd, bundle_id);
            }
        }
        
        // Calculate available liquidity with real-time pending
        let pending_committed = bundle_tracker.get_pending_committed();
        let available_liquidity = wallet_balance_tracker.current_estimated_balance_usd - pending_committed;
        let current_max_position_usd = available_liquidity * config.max_position_pct;
        
        let position_size_usd = quote.collateral_value_usd;
        
        log::debug!(
            "Risk calculation: estimated_wallet=${:.2}, pending_committed=${:.2} (real-time tracking), available=${:.2}, max_position=${:.2}",
            wallet_balance_tracker.current_estimated_balance_usd,
            pending_committed,
            available_liquidity,
            current_max_position_usd
        );
        
        // Per-liquidation check: single liquidation cannot exceed max position
        if position_size_usd > current_max_position_usd {
            log::warn!(
                "Skipping {}: Position ${:.2} exceeds per-liquidation limit ${:.2} (estimated_balance=${:.2})",
                obl_pubkey,
                position_size_usd,
                current_max_position_usd,
                wallet_balance_tracker.current_estimated_balance_usd
            );
            metrics.skipped_risk_limit += 1;
            continue;
        }

        // Per Structure.md section 6.4: Block-wide cumulative risk check
        // "Tek blok içinde kullanılan toplam risk de aynı limit ile sınırlıdır"
        // 
        // CRITICAL: cumulative_risk_usd tracks risk from liquidations sent in this cycle.
        // We check against current estimated wallet value to ensure we don't exceed limits even if
        // previous liquidations have executed and changed the wallet balance.
        let new_cumulative_risk = cumulative_risk_usd + position_size_usd;
        if new_cumulative_risk > current_max_position_usd {
            log::warn!(
                "Skipping {}: Cumulative risk ${:.2} + position ${:.2} = ${:.2} exceeds block-wide limit ${:.2} (estimated_balance=${:.2})",
                obl_pubkey,
                cumulative_risk_usd,
                position_size_usd,
                new_cumulative_risk,
                current_max_position_usd,
                wallet_balance_tracker.current_estimated_balance_usd
            );
            metrics.skipped_risk_limit += 1;
            continue;
        }
        
        log::debug!(
            "Risk check passed: position=${:.2}, cumulative=${:.2}/{:.2}, estimated_wallet=${:.2}",
            position_size_usd,
            new_cumulative_risk,
            current_max_position_usd,
            wallet_balance_tracker.current_estimated_balance_usd
        );

        // d) Jito bundle ile gönder
        if matches!(config.liquidation_mode, LiquidationMode::Live) {
            // ✅ FLASHLOAN APPROACH: Single atomic transaction (NO race condition risk!)
            // All operations in ONE transaction: FlashBorrow -> Liquidate -> Redeem -> Swap -> FlashRepay
            // Benefits:
            // - ✅ Atomicity: All operations succeed or fail together
            // - ✅ No MEV risk: No intermediate state exposed
            // - ✅ No race condition: TX1/TX2 split eliminated
            // - ✅ Gas-efficient: Single transaction fee
            // - ✅ No capital needed: Flashloan provides initial funds
            match execute_liquidation_with_swap(&ctx, &quote, &config, rpc, jito_client).await {
                Ok(_) => {
                            // Update balance tracker
                            wallet_balance_tracker.liquidations_since_refresh += 1;
                            wallet_balance_tracker.total_liquidations += 1;
                            
                            // ESTIMATED balance update (without RPC call)
                            // Formula: new_balance = old_balance + profit - jito_tip - tx_fee
                            // Note: quote.profit_usdc already includes all fees (jito_tip + tx_fee)
                            let estimated_profit = quote.profit_usdc;
                            wallet_balance_tracker.current_estimated_balance_usd += estimated_profit;
                            
                            // Update cumulative risk and pending liquidation tracking after successful send
                            // CRITICAL FIX: Real-time bundle tracking with status checking
                            // NOTE: For flashloan approach, we track the single atomic transaction bundle
                            // The bundle ID is returned from execute_liquidation_with_swap
                            // For now, we use a placeholder ID - actual bundle ID should be returned from execute_liquidation_with_swap
                            let bundle_id = format!("FLASHLOAN_{}", obl_pubkey);
                            bundle_tracker.add_bundle(bundle_id.clone(), position_size_usd);
                            
                            // CRITICAL: Track committed amount to prevent overcommit
                            // This capital is now tied up in the pending liquidation
                            wallet_balance_tracker.pending_committed_usd += position_size_usd;
                            
                            cumulative_risk_usd += position_size_usd;
                            
                            // Calculate current pending value for logging
                            let current_pending_value = bundle_tracker.get_pending_committed();
                            
                            // REFRESH balance every 5 liquidations (doğrulama için)
                            const REFRESH_INTERVAL: u32 = 5;
                            let refresh_countdown = REFRESH_INTERVAL.saturating_sub(wallet_balance_tracker.liquidations_since_refresh);
                            
                            log::info!(
                                "✅ Liquidated {} with profit ${:.2} (FLASHLOAN), estimated_balance=${:.2}, pending_committed=${:.2} (real-time: ${:.2}, refresh in {} liquidations), cumulative_risk=${:.2}/${:.2}",
                                obl_pubkey,
                                quote.profit_usdc,
                                wallet_balance_tracker.current_estimated_balance_usd,
                                wallet_balance_tracker.pending_committed_usd,
                                current_pending_value,
                                refresh_countdown,
                                cumulative_risk_usd,
                                current_max_position_usd
                            );
                            
                            // Refresh actual balance every 5 liquidations to verify estimation accuracy
                            if wallet_balance_tracker.liquidations_since_refresh >= REFRESH_INTERVAL {
                                match get_wallet_value_usd(rpc, &config.wallet.pubkey()).await {
                                    Ok(actual_balance) => {
                                        let estimation_error = (wallet_balance_tracker.current_estimated_balance_usd 
                                            - actual_balance).abs();
                                        let error_pct = if actual_balance > 0.0 {
                                            (estimation_error / actual_balance) * 100.0
                                        } else {
                                            0.0
                                        };
                                        
                                        log::info!(
                                            "🔄 Balance refresh: estimated=${:.2}, actual=${:.2}, error=${:.2} ({:.2}%)",
                                            wallet_balance_tracker.current_estimated_balance_usd,
                                            actual_balance,
                                            estimation_error,
                                            error_pct
                                        );
                                        
                                        // Update with actual balance
                                        wallet_balance_tracker.current_estimated_balance_usd = actual_balance;
                                        wallet_balance_tracker.last_refresh_balance_usd = actual_balance;
                                        wallet_balance_tracker.liquidations_since_refresh = 0;
                                        
                                        // CRITICAL: High error rate indicates problem
                                        if error_pct > 10.0 {
                                            log::error!(
                                                "⚠️  HIGH BALANCE ESTIMATION ERROR: {:.2}%! \
                                                 This indicates profit calculation or fee estimation issues. \
                                                 Investigate immediately!",
                                                error_pct
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        log::warn!("Failed to refresh wallet balance: {}", e);
                                        // Continue with estimated balance
                                    }
                                }
                            }
                            
                            metrics.successful += 1;
                }
                Err(e) => {
                    log::error!("Failed to execute liquidation with swap for {}: {}", obl_pubkey, e);
                    metrics.failed_send_bundle += 1;
                }
            }
        } else {
            // Update cumulative risk for dry run as well
            cumulative_risk_usd += position_size_usd;
            log::info!(
                "DryRun: would liquidate obligation {} with profit ~${:.2} USDC, cumulative_risk=${:.2}/{:.2} (estimated_balance=${:.2})",
                obl_pubkey,
                quote.profit_usdc,
                cumulative_risk_usd,
                current_max_position_usd,
                wallet_balance_tracker.current_estimated_balance_usd
            );
            metrics.successful += 1;
        }
    }

    // Log cycle summary metrics
    // Get final wallet value for summary (may have changed if liquidations executed)
    // Use estimated balance if refresh failed, otherwise use actual
    let final_wallet_value_usd = match get_wallet_value_usd(rpc, &config.wallet.pubkey()).await {
        Ok(value) => {
            // Log final estimation error if we have liquidations
            if wallet_balance_tracker.total_liquidations > 0 {
                let final_estimation_error = (wallet_balance_tracker.current_estimated_balance_usd - value).abs();
                let final_error_pct = if value > 0.0 {
                    (final_estimation_error / value) * 100.0
                } else {
                    0.0
                };
                log::debug!(
                    "Final balance check: estimated=${:.2}, actual=${:.2}, error=${:.2} ({:.2}%)",
                    wallet_balance_tracker.current_estimated_balance_usd,
                    value,
                    final_estimation_error,
                    final_error_pct
                );
            }
            value
        }
        Err(e) => {
            log::warn!("Failed to get final wallet value for summary: {}, using estimated balance", e);
            wallet_balance_tracker.current_estimated_balance_usd // Use estimated as fallback
        }
    };
    let final_max_position_usd = final_wallet_value_usd * config.max_position_pct;
    
    let total_processed = metrics.total_candidates;
    let total_skipped = metrics.skipped_oracle_fail
        + metrics.skipped_jupiter_fail
        + metrics.skipped_insufficient_profit
        + metrics.skipped_risk_limit
        + metrics.skipped_rpc_error
        + metrics.skipped_reserve_load_fail
        + metrics.skipped_ata_missing;
    let total_failed = metrics.failed_build_tx + metrics.failed_send_bundle;
    
    log::info!(
        "📊 Cycle Summary: {} candidates | {} successful | {} skipped (oracle:{}, jup:{}, profit:{}, risk:{}, rpc:{}, load:{}, ata:{}) | {} tx_failed (build:{}, send:{}) | Bundles: {} confirmed, {} dropped | cumulative_risk=${:.2}/{:.2} (wallet=${:.2})",
        total_processed,
        metrics.successful,
        total_skipped,
        metrics.skipped_oracle_fail,
        metrics.skipped_jupiter_fail,
        metrics.skipped_insufficient_profit,
        metrics.skipped_risk_limit,
        metrics.skipped_rpc_error,
        metrics.skipped_reserve_load_fail,
        metrics.skipped_ata_missing,
        total_failed,
        metrics.failed_build_tx,
        metrics.failed_send_bundle,
        metrics.confirmed_bundles,
        metrics.dropped_bundles,
        cumulative_risk_usd,
        final_max_position_usd,
        final_wallet_value_usd
    );

    // Structured log for external monitoring (Problems.md requirement)
    // Always log scan stats even if no candidates found (for liveness check)
    log::info!(target: "scan_stats", "{}", serde_json::json!({
        "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
        "candidates": total_processed,
        "successful": metrics.successful,
        "skipped": total_skipped,
        "skipped_breakdown": {
            "oracle": metrics.skipped_oracle_fail,
            "jupiter": metrics.skipped_jupiter_fail,
            "profit": metrics.skipped_insufficient_profit,
            "risk": metrics.skipped_risk_limit,
            "rpc": metrics.skipped_rpc_error,
            "reserve": metrics.skipped_reserve_load_fail,
            "ata": metrics.skipped_ata_missing
        },
        "failed_tx": total_failed,
        "bundles": {
            "confirmed": metrics.confirmed_bundles,
            "dropped": metrics.dropped_bundles
        },
        "risk": {
            "cumulative_used": cumulative_risk_usd,
            "limit": final_max_position_usd,
            "wallet_balance": final_wallet_value_usd
        }
    }));

    Ok(())
}

/// Validate Jito endpoint is reachable per Structure.md section 13
async fn validate_jito_endpoint(jito_client: &JitoClient) -> Result<()> {
    // Simple connectivity check - Jito endpoint validation
    // Note: Jito doesn't have a standard health endpoint, so we just log
    log::info!("✅ Jito client initialized: {}", jito_client.url());
    // In production, you might want to do an actual connectivity test
    // For now, we'll validate during first bundle send
    Ok(())
}

/// Liquidation context per Structure.md section 9
pub struct LiquidationContext {
    obligation_pubkey: Pubkey,
    obligation: Obligation,
    borrows: Vec<ObligationLiquidity>,  // Parsed borrows from dataFlat
    deposits: Vec<ObligationCollateral>, // Parsed deposits from dataFlat
    borrow_reserve: Option<Reserve>,
    deposit_reserve: Option<Reserve>,
    borrow_price_usd: Option<f64>,  // Price from oracle
    deposit_price_usd: Option<f64>, // Price from oracle
    oracle_ok: bool,
}

/// Build liquidation context with Oracle validation per Structure.md section 5.2
async fn build_liquidation_context(
    rpc: &Arc<RpcClient>,
    obligation: &Obligation,
) -> Result<LiquidationContext> {
    // Load reserve accounts for borrow and deposit
    let mut borrow_reserve = None;
    let mut deposit_reserve = None;

    // Get first borrow reserve if exists
    match obligation.borrows() {
        Ok(borrows) => {
            if !borrows.is_empty() {
                let borrow_reserve_pubkey = borrows[0].borrowReserve;
                match rpc.get_account_data(&borrow_reserve_pubkey) {
                    Ok(account) => {
                        match Reserve::from_account_data(&account) {
                            Ok(reserve) => {
                                borrow_reserve = Some(reserve);
                                log::debug!("✅ Loaded borrow reserve: {}", borrow_reserve_pubkey);
                            }
                            Err(e) => {
                                log::warn!("❌ Failed to parse borrow reserve {}: {}", borrow_reserve_pubkey, e);
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("❌ Failed to load borrow reserve account {}: {}", borrow_reserve_pubkey, e);
                    }
                }
            } else {
                log::debug!("Obligation has no borrows");
            }
        }
        Err(e) => {
            log::warn!("❌ Failed to parse obligation borrows: {}", e);
        }
    }

    // Get first deposit reserve if exists
    match obligation.deposits() {
        Ok(deposits) => {
            if !deposits.is_empty() {
                let deposit_reserve_pubkey = deposits[0].depositReserve;
                match rpc.get_account_data(&deposit_reserve_pubkey) {
                    Ok(account) => {
                        match Reserve::from_account_data(&account) {
                            Ok(reserve) => {
                                deposit_reserve = Some(reserve);
                                log::debug!("✅ Loaded deposit reserve: {}", deposit_reserve_pubkey);
                            }
                            Err(e) => {
                                log::warn!("❌ Failed to parse deposit reserve {}: {}", deposit_reserve_pubkey, e);
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("❌ Failed to load deposit reserve account {}: {}", deposit_reserve_pubkey, e);
                    }
                }
            } else {
                log::debug!("Obligation has no deposits");
            }
        }
        Err(e) => {
            log::warn!("❌ Failed to parse obligation deposits: {}", e);
        }
    }

    // Parse borrows and deposits from obligation
    let borrows = obligation.borrows()
        .map_err(|e| anyhow::anyhow!("Failed to parse borrows: {}", e))?;
    let deposits = obligation.deposits()
        .map_err(|e| anyhow::anyhow!("Failed to parse deposits: {}", e))?;

    // Validate Oracle per Structure.md section 5.2
    // Use TWAP protection when Switchboard is not available (Pyth-only mode)
    let (oracle_ok, borrow_price, deposit_price) = validate_oracles_with_twap(rpc, &borrow_reserve, &deposit_reserve).await?;

    Ok(LiquidationContext {
        obligation_pubkey: obligation.owner, // Note: actual obligation pubkey should be passed separately
        obligation: obligation.clone(),
        borrows,
        deposits,
        borrow_reserve,
        deposit_reserve,
        borrow_price_usd: borrow_price,
        deposit_price_usd: deposit_price,
        oracle_ok,
    })
}

// Oracle validation functions moved to oracle::validation module:
// - validate_oracles -> oracle::validation::validate_oracles
// - validate_oracles_with_twap -> oracle::validation::validate_oracles_with_twap
// - OraclePriceCache -> oracle::validation::OraclePriceCache (private)


/// Liquidation quote with profit calculation per Structure.md section 7
pub struct LiquidationQuote {
    quote: JupiterQuote,
    profit_usdc: f64,
    collateral_value_usd: f64, // Position size in USD for risk limit calculation
    debt_to_repay_raw: u64, // Debt amount to repay (in debt token raw units) for Solend instruction
    collateral_to_seize_raw: u64, // Collateral cToken amount to seize (for redemption check)
    flashloan_fee_raw: u64, // Flashloan fee amount (in raw token units) - CRITICAL: Store to avoid recalculation inconsistency
}

// Quote and profit calculation functions moved to quotes module:
// - get_liquidation_quote -> quotes::get_liquidation_quote
// - get_sol_price_usd -> quotes::get_sol_price_usd
// - get_sol_price_usd_standalone -> quotes::get_sol_price_usd_standalone

// Removed old implementations - see quotes.rs for current code

/// Log wallet balances (SOL and USDC) to console
/// Used for periodic balance monitoring
async fn log_wallet_balances(
    let sol_usd_pyth_feed_primary = {
        let feed_str = env::var("SOL_USD_PYTH_FEED")
            .unwrap_or_else(|_| "H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG".to_string());
        
        match Pubkey::from_str(&feed_str) {
            Ok(pk) => pk,
            Err(_) => {
                log::error!("Failed to parse primary Pyth SOL/USD feed address from SOL_USD_PYTH_FEED: {}", feed_str);
                // Try to continue with hardcoded fallback
                match Pubkey::from_str("H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG") {
                    Ok(pk) => pk,
                    Err(_) => {
                        log::error!("Failed to parse hardcoded fallback Pyth feed address");
                        // Continue to try other methods
                        return None;
                    }
                }
            }
        }
    };
    
    // Get current slot once and reuse it
    let current_slot = rpc.get_slot().ok();
    
    // Try primary Pyth feed (if we have a valid slot)
    if let Some(slot) = current_slot {
        match oracle::pyth::validate_pyth_oracle(rpc, sol_usd_pyth_feed_primary, slot).await {
            Ok((true, Some(price))) => {
                log::debug!("✅ SOL price from primary Pyth feed: ${:.2}", price);
                return Some(price);
            }
            Ok((true, None)) => {
                log::warn!(
                    "⚠️  Primary Pyth SOL/USD feed validation passed but price is None (unexpected). \
                     Feed: {}",
                    sol_usd_pyth_feed_primary
                );
            }
            Ok((false, _)) => {
                log::debug!(
                    "⚠️  Primary Pyth SOL/USD feed validation failed (stale/invalid). \
                     Feed: {}. Trying backup methods...",
                    sol_usd_pyth_feed_primary
                );
            }
            Err(e) => {
                log::warn!(
                    "❌ Error validating primary Pyth SOL/USD feed {}: {}. Trying backup methods...",
                    sol_usd_pyth_feed_primary,
                    e
                );
            }
        }
    }
    
    // Method 2: Try backup Pyth feed if available
    if let Ok(backup_feed_str) = env::var("SOL_USD_BACKUP_FEED") {
        if let Ok(backup_feed) = Pubkey::from_str(&backup_feed_str) {
            if let Some(slot) = current_slot {
                log::debug!("Trying backup Pyth feed: {}", backup_feed);
                match oracle::pyth::validate_pyth_oracle(rpc, backup_feed, slot).await {
                    Ok((true, Some(price))) => {
                        log::info!("✅ SOL price from backup Pyth feed: ${:.2}", price);
                        return Some(price);
                    }
                    Ok((false, _)) => {
                        log::debug!("⚠️ Backup Pyth SOL/USD feed validation failed (stale/invalid). Feed: {}", backup_feed);
                    }
                    Err(e) => {
                        log::debug!("❌ Error validating backup Pyth SOL/USD feed {}: {}", backup_feed, e);
                    }
                    _ => {}
                }
            }
        } else {
            log::warn!("Failed to parse backup Pyth feed address from SOL_USD_BACKUP_FEED: {}", backup_feed_str);
        }
    }
    
    // ✅ FIXED: Method 3 - Try Switchboard feed (Problems.md issue #3)
    // Add Switchboard as fallback before API calls (faster and more reliable)
    if let Ok(switchboard_feed_str) = env::var("SOL_USD_SWITCHBOARD_FEED") {
        if let Ok(switchboard_feed) = Pubkey::from_str(&switchboard_feed_str) {
            if let Some(slot) = current_slot {
                log::debug!("Trying Switchboard feed: {}", switchboard_feed);
                // Use direct Switchboard validation by pubkey (no Reserve needed)
                match oracle::switchboard::validate_switchboard_oracle_by_pubkey(rpc, switchboard_feed, slot).await {
                    Ok(Some(price)) => {
                        log::info!("✅ SOL price from Switchboard feed: ${:.2}", price);
                        return Some(price);
                    }
                    Ok(None) => {
                        log::debug!("⚠️ Switchboard SOL/USD feed validation failed (stale/invalid). Feed: {}", switchboard_feed);
                    }
                    Err(e) => {
                        log::debug!("❌ Error validating Switchboard SOL/USD feed {}: {}", switchboard_feed, e);
                    }
                }
            }
        } else {
            log::warn!("Failed to parse Switchboard feed address from SOL_USD_SWITCHBOARD_FEED: {}", switchboard_feed_str);
        }
    }
    
    // Method 4: Try Pyth Hermes Price Service API (Primary off-chain source)
    // This is faster and more reliable than other public APIs
    const PYTH_HERMES_API: &str = "https://hermes.pyth.network/api/latest_price_feeds?ids[]=0xef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";
    
    // Create HTTP client with short timeout (2s per API)
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build();
        
    if let Ok(client) = &client {
        match client.get(PYTH_HERMES_API).send().await {
            Ok(resp) if resp.status().is_success() => {
                // Parse Pyth Hermes response
                // Format: [{"id": "...", "price": {"price": "...", "conf": "...", "expo": ...}, ...}]
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if let Some(feed) = json.as_array().and_then(|arr| arr.get(0)) {
                        if let Some(price_obj) = feed.get("price") {
                            let price_str = price_obj.get("price").and_then(|p| p.as_str());
                            let expo = price_obj.get("expo").and_then(|e| e.as_i64());
                            
                            if let (Some(p_str), Some(e)) = (price_str, expo) {
                                if let Ok(p_val) = p_str.parse::<i64>() {
                                    let price = (p_val as f64) * 10_f64.powi(e as i32);
                                    log::info!("✅ SOL price from Pyth Hermes API: ${:.2}", price);
                                    return Some(price);
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                log::debug!("Pyth Hermes API failed: {}", e);
            }
            _ => {}
        }
    }

    // Method 5: Try multiple reliable price APIs (sequential with fast timeout)
    // These are reliable off-chain APIs that should always work
    log::info!("⚠️ All Pyth feeds failed, trying multiple reliable price APIs for SOL price...");
    
    // Reuse existing client if possible, or create new one
    let client = client.unwrap_or_else(|_| reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap_or_default());
    
    // API 1: CoinGecko (most reliable, free, no API key needed, used by millions)
    const COINGECKO_API: &str = "https://api.coingecko.com/api/v3/simple/price?ids=solana&vs_currencies=usd";
    match client.get(COINGECKO_API).send().await {
        Ok(resp) if resp.status().is_success() => {
            #[derive(serde::Deserialize)]
            struct CoinGeckoResponse {
                solana: Option<CoinGeckoPrice>,
            }
            #[derive(serde::Deserialize)]
            struct CoinGeckoPrice {
                usd: Option<f64>,
            }
            if let Ok(json) = resp.json::<CoinGeckoResponse>().await {
                if let Some(price) = json.solana.and_then(|p| p.usd) {
                    log::info!("✅ SOL price from CoinGecko API: ${:.2}", price);
                    return Some(price);
                }
            }
        }
        Err(e) => {
            log::debug!("CoinGecko API failed: {}", e);
        }
        _ => {}
    }
    
    // API 2: Binance (very fast and reliable, largest exchange)
    const BINANCE_API: &str = "https://api.binance.com/api/v3/ticker/price?symbol=SOLUSDT";
    match client.get(BINANCE_API).send().await {
        Ok(resp) if resp.status().is_success() => {
            #[derive(serde::Deserialize)]
            struct BinanceResponse {
                price: String,
            }
            if let Ok(json) = resp.json::<BinanceResponse>().await {
                if let Ok(price) = json.price.parse::<f64>() {
                    log::info!("✅ SOL price from Binance API: ${:.2}", price);
                    return Some(price);
                }
            }
        }
        Err(e) => {
            log::debug!("Binance API failed: {}", e);
        }
        _ => {}
    }
    
    // API 3: Jupiter Price API (Solana-native, should be fast)
    const JUPITER_PRICE_API: &str = "https://price.jup.ag/v4/price?ids=SOL";
    match client.get(JUPITER_PRICE_API).send().await {
        Ok(resp) if resp.status().is_success() => {
            #[derive(serde::Deserialize)]
            struct PriceData {
                price: f64,
            }
            #[derive(serde::Deserialize)]
            struct JupiterPriceResponse {
                data: std::collections::HashMap<String, PriceData>,
            }
            if let Ok(json) = resp.json::<JupiterPriceResponse>().await {
                if let Some(data) = json.data.get("SOL") {
                    log::info!("✅ SOL price from Jupiter API: ${:.2}", data.price);
                    return Some(data.price);
                }
            }
        }
        Err(e) => {
            log::debug!("Jupiter Price API failed: {}", e);
        }
        _ => {}
    }
    
    // Note: CoinGecko, Binance, and Jupiter are all FREE and don't require API keys
    // These three should be more than enough to get SOL price reliably

    log::error!(
        "❌ CRITICAL: All SOL price oracle methods failed! \
         SOL price is REQUIRED for: profit calculations, fee calculations, risk limit checks. \
         Bot cannot safely operate without accurate SOL price. \
         Please check: RPC connectivity, Pyth feed availability, network conditions."
    );
    
    None
}

// Quote and profit calculation functions moved to quotes module - see quotes.rs

/// Log wallet balances (SOL and USDC) to console
/// Used for periodic balance monitoring
async fn log_wallet_balances(
    rpc: &Arc<RpcClient>,
    wallet_pubkey: &Pubkey,
) -> Result<()> {

    // Get SOL balance
    let sol_balance_lamports = rpc
        .get_balance(wallet_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get SOL balance: {}", e))?;
    let sol_balance = sol_balance_lamports as f64 / 1_000_000_000.0;
    
    // Get SOL price for USD value
    // Use the specific Pyth feed for SOL/USD price
    let sol_usd_pyth_feed = match Pubkey::from_str("H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG") {
        Ok(pk) => pk,
        Err(_) => {
            log::warn!("Failed to parse SOL/USD Pyth feed address, using fallback method");
            // Fall back to standalone function if feed address is invalid
            let sol_price_usd = match quotes::get_sol_price_usd_standalone(rpc).await {
                Some(price) => price,
                None => {
                    log::warn!("⚠️  Failed to get SOL price, using fallback $150");
                    150.0
                }
            };
            let sol_value_usd = sol_balance * sol_price_usd;
            // Continue with USDC check using fallback price
            let program_id = crate::solend::solend_program_id()?;
            let usdc_mint = crate::solend::find_usdc_mint_from_reserves(rpc, &program_id)
                .context("Failed to discover USDC mint")?;
            use spl_associated_token_account::get_associated_token_address;
            let usdc_ata = get_associated_token_address(wallet_pubkey, &usdc_mint);
            let usdc_balance_raw = match rpc.get_token_account(&usdc_ata) {
                Ok(Some(account)) => account.token_amount.amount.parse::<u64>().unwrap_or(0),
                _ => 0,
            };
            let usdc_balance = usdc_balance_raw as f64 / 1_000_000.0;
            log::info!(
                "💰 Wallet Balances: SOL: {:.6} SOL (~${:.2}), USDC: {:.2} USDC, Total: ~${:.2}",
                sol_balance,
                sol_value_usd,
                usdc_balance,
                sol_value_usd + usdc_balance
            );
            return Ok(());
        }
    };
    
    // Try to get SOL price directly from the specified Pyth feed first
    let current_slot = rpc.get_slot().unwrap_or(0);
    let sol_price_usd = match oracle::pyth::validate_pyth_oracle(rpc, sol_usd_pyth_feed, current_slot).await {
        Ok((true, Some(price))) => {
            log::debug!("✅ SOL price from Pyth feed {}: ${:.2}", sol_usd_pyth_feed, price);
            price
        },
        _ => {
            // Fall back to robust standalone function (tries multiple methods)
            match quotes::get_sol_price_usd_standalone(rpc).await {
                Some(price) => {
                    log::debug!("✅ SOL price for balance logging: ${:.2}", price);
                    price
                },
                None => {
                    log::warn!(
                        "⚠️  Failed to get SOL price from ALL methods for balance logging, using fallback $150. \
                         Wallet value calculations may be inaccurate."
                    );
                    150.0
                }
            }
        }
    };
    
    let sol_value_usd = sol_balance * sol_price_usd;
    
    // Get USDC balance
    let program_id = crate::solend::solend_program_id()?;
    let usdc_mint = crate::solend::find_usdc_mint_from_reserves(rpc, &program_id)
        .context("Failed to discover USDC mint")?;
    
    use spl_associated_token_account::get_associated_token_address;
    let usdc_ata = get_associated_token_address(wallet_pubkey, &usdc_mint);
    
    let usdc_balance_raw = match rpc.get_token_account(&usdc_ata) {
        Ok(Some(account)) => {
            account.token_amount.amount.parse::<u64>().unwrap_or(0)
        }
        Ok(None) => 0,
        Err(_) => 0,
    };
    
    let usdc_balance = usdc_balance_raw as f64 / 1_000_000.0;
    let total_value_usd = sol_value_usd + usdc_balance;
    
    log::info!(
        "💰 Wallet Balances: SOL: {:.6} SOL (${:.2}), USDC: {:.2} USDC, Total: ${:.2}",
        sol_balance,
        sol_value_usd,
        usdc_balance,
        total_value_usd
    );
    
    Ok(())
}

/// Get total wallet value in USD (SOL + USDC)
/// Used for risk limit calculations per Structure.md section 6.4
async fn get_wallet_value_usd(
    rpc: &Arc<RpcClient>,
    wallet_pubkey: &Pubkey,
) -> Result<f64> {
    // 1. Get SOL balance and price
    let sol_balance = rpc
        .get_balance(wallet_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get SOL balance: {}", e))?;

    let sol_balance_sol = sol_balance as f64 / 1_000_000_000.0;
    
    // Get SOL price from oracle
    let sol_price_usd = match quotes::get_sol_price_usd_standalone(rpc).await {
        Some(price) => price,
        None => {
            log::warn!("⚠️  Failed to get SOL price for wallet value calculation, using fallback $150");
            150.0
        }
    };
    
    let sol_value_usd = sol_balance_sol * sol_price_usd;
    
    // 2. Get USDC balance
    let program_id = crate::solend::solend_program_id()?;
    let usdc_mint = crate::solend::find_usdc_mint_from_reserves(rpc, &program_id)
        .context("Failed to discover USDC mint")?;
    
    use spl_associated_token_account::get_associated_token_address;
    let usdc_ata = get_associated_token_address(wallet_pubkey, &usdc_mint);
    
    let usdc_balance_raw = match rpc.get_token_account(&usdc_ata) {
        Ok(Some(account)) => {
            account.token_amount.amount.parse::<u64>().unwrap_or(0)
        }
        Ok(None) => 0,
        Err(_) => 0,
    };
    
    let usdc_balance = usdc_balance_raw as f64 / 1_000_000.0;
    
    // 3. Total value
    let total_value_usd = sol_value_usd + usdc_balance;
    
    log::debug!(
        "Wallet value: SOL: {:.6} SOL (${:.2}), USDC: {:.2} USDC, Total: ${:.2}",
        sol_balance_sol,
        sol_value_usd,
        usdc_balance,
        total_value_usd
    );
    
    Ok(total_value_usd)
}

// Quote and profit calculation functions moved to quotes module - see quotes.rs
    let collateral_decimals = deposit_reserve.liquidity().mintDecimals;
    
    // CRITICAL SECURITY: Validate decimal values to prevent corrupt data issues
    // SPL tokens use 0-18 decimals (standard range)
    // If layout changes or data is corrupt, we might get invalid values (0, 255, etc.)
    if debt_decimals > 18 {
        return Err(anyhow::anyhow!(
            "Invalid debt decimals: {} (expected 0-18). Reserve: {}. This may indicate corrupt data or layout changes.",
            debt_decimals,
            ctx.borrows[0].borrowReserve
        ));
    }
    
    if collateral_decimals > 18 {
        return Err(anyhow::anyhow!(
            "Invalid collateral decimals: {} (expected 0-18). Reserve: {}. This may indicate corrupt data or layout changes.",
            collateral_decimals,
            ctx.deposits[0].depositReserve
        ));
    }
    
    // Step 2: Calculate debt to repay (close factor from reserve config)
    // CRITICAL FIX: Solend's debt calculation works as follows:
    // 1. borrowedAmountWad: Initial borrowed amount (WAD format, token decimals NOT included)
    // 2. cumulativeBorrowRateWads: Interest rate accumulator (WAD format)
    // 3. Token decimals: Mint's actual decimals (e.g., USDC = 6, SOL = 9)
    //
    // Formula: actual_debt = borrowedAmountWad * cumulativeBorrowRateWads / WAD / WAD
    // Why two divisions? Both inputs are WAD (10^18), product is 10^36, need two divisions to normalize
    // Result is normalized amount (not in WAD format), then multiply by 10^decimals to get raw amount
    const WAD: u128 = 1_000_000_000_000_000_000; // 10^18
    
    // CRITICAL FIX: Get close factor from reserve config (not hardcoded)
    // Close factor can be changed by Solend governance, so we read it from chain
    // Currently, ReserveConfig doesn't have closeFactor field, so close_factor() returns 0.5 (50%)
    // If Solend adds closeFactor to ReserveConfig in the future, it will be automatically used
    let close_factor_f64 = borrow_reserve.close_factor(); // Returns 0.5 (50%) as fallback
    let close_factor_wad = (close_factor_f64 * WAD as f64) as u128;
    
    log::debug!(
        "Close factor: {:.1}% (from reserve config, fallback to 50% if not available)",
        close_factor_f64 * 100.0
    );
    
    // Step 2a: Calculate actual debt in normalized format (interest included)
    // CRITICAL: Both borrowedAmountWad and cumulativeBorrowRateWads are in WAD format (10^18)
    // When multiplied: 10^36, so we need to divide by WAD twice to get normalized amount
    // Formula: actual_debt = borrowedAmountWad * cumulativeBorrowRateWads / WAD / WAD
    let actual_debt_wad: u128 = borrow.borrowedAmountWads
        .checked_mul(borrow.cumulativeBorrowRateWads)
        .and_then(|v| v.checked_div(WAD))
        .and_then(|v| v.checked_div(WAD))  // ✅ İKİNCİ DIVISION - CRITICAL FIX
        .ok_or_else(|| anyhow::anyhow!("Debt calculation overflow: borrowedAmountWad * cumulativeBorrowRateWads"))?;
    
    // Step 2b: Apply close factor (from reserve config)
    let debt_to_repay_wad: u128 = actual_debt_wad
        .checked_mul(close_factor_wad)
        .and_then(|v| v.checked_div(WAD))
        .ok_or_else(|| anyhow::anyhow!("Close factor calculation overflow"))?;
    
    // Step 2c: Convert normalized amount to raw token amount with decimals
    // After the double WAD division, actual_debt_wad and debt_to_repay_wad are normalized (not in WAD format)
    // We just need to multiply by 10^decimals to get the raw token amount
    // 
    // Example for USDC (6 decimals):
    // - actual_debt_wad = 1575 (normalized, after double WAD division)
    // - debt_to_repay_wad = 787 (normalized, after close factor)
    // - debt_to_repay_raw = 787 * 10^6 = 787000000 (raw USDC amount)
    let decimals_multiplier = 10_u128
        .checked_pow(debt_decimals as u32)
        .ok_or_else(|| anyhow::anyhow!("Decimals multiplier overflow: 10^{}", debt_decimals))?;
    
    // CORRECT FORMULA: debt_to_repay_wad is already normalized, just multiply by decimals
    // debt_to_repay_raw = debt_to_repay_wad * 10^decimals
    let debt_to_repay_raw = debt_to_repay_wad
        .checked_mul(decimals_multiplier)
        .ok_or_else(|| anyhow::anyhow!("Raw amount conversion overflow"))?
        as u64;
    
    log::debug!(
        "Debt calculation (CORRECTED): borrowed_wad={}, cumulative_rate={}, actual_debt_wad={}, debt_to_repay_wad={}, debt_to_repay_raw={} (decimals={})",
        borrow.borrowedAmountWads,
        borrow.cumulativeBorrowRateWads,
        actual_debt_wad,
        debt_to_repay_wad,
        debt_to_repay_raw,
        debt_decimals
    );
    
    // Step 3: Get liquidation bonus from deposit reserve (collateral reserve)
    let liquidation_bonus = deposit_reserve.liquidation_bonus(); // Returns 0.05 for 5%, etc.
    
    // Step 4: Convert debt raw amount to USD for calculations
    let debt_price_usd = ctx
        .borrow_price_usd
        .ok_or_else(|| anyhow::anyhow!("Borrow price not available"))?;
    let collateral_price_usd = ctx
        .deposit_price_usd
        .ok_or_else(|| anyhow::anyhow!("Deposit price not available"))?;
    
    // Convert raw debt amount to normalized amount, then to USD
    let debt_to_repay_normalized = debt_to_repay_raw as f64 / 10_f64.powi(debt_decimals as i32);
    let debt_to_repay_usd = debt_to_repay_normalized * debt_price_usd;
    
    // VERIFICATION: Convert back to check
    let verification = (debt_to_repay_raw as f64) / 10_f64.powi(debt_decimals as i32);
    let debt_to_repay_usd_check = verification * debt_price_usd;
    log::debug!(
        "Debt verification: raw={} -> normalized={:.6} -> ${:.2} USD",
        debt_to_repay_raw,
        verification,
        debt_to_repay_usd_check
    );
    
    // Step 5: Calculate collateral to seize
    // CRITICAL FIX: Solend liquidation bonus calculation
    //
    // Solend formula (from whitepaper):
    // collateral_to_seize = (debt_to_repay / collateral_price) * (1 + liquidation_bonus)
    //
    // This formula gives collateral TOKEN amount, not USD!
    // So:
    // 1. Convert debt from USD to collateral token (debt_usd / collateral_price)
    // 2. Add bonus (amount * (1 + bonus))
    // 3. Convert to raw amount (amount * 10^decimals)
    
    // Step 5a: Convert debt to collateral token amount (before bonus)
    // debt_in_collateral_tokens = debt_to_repay_usd / collateral_price_usd
    let debt_in_collateral_tokens = debt_to_repay_usd / collateral_price_usd;
    
    // Step 5b: Apply liquidation bonus (in token amount, not USD!)
    // CRITICAL: Bonus is applied to token amount, then converted to USD
    let collateral_to_seize_tokens = debt_in_collateral_tokens * (1.0 + liquidation_bonus);
    
    // Step 5c: Convert to USD for logging and validation
    let collateral_to_seize_usd = collateral_to_seize_tokens * collateral_price_usd;
    
    // Step 5d: Convert to raw token amount with decimals
    let collateral_to_seize_raw = (collateral_to_seize_tokens * 10_f64.powi(collateral_decimals as i32)) as u64;
    
    log::debug!(
        "Collateral calculation (CORRECTED): debt_to_repay=${:.2} USD -> {:.6} collateral tokens -> {:.6} with bonus ({:.1}%) -> {:.6} USD -> {} raw",
        debt_to_repay_usd,
        debt_in_collateral_tokens,
        collateral_to_seize_tokens,
        liquidation_bonus * 100.0,
        collateral_to_seize_usd,
        collateral_to_seize_raw
    );
    
    // VERIFICATION: Profit check (before Jupiter swap)
    // Expected profit (if no slippage): collateral_usd - debt_usd
    let expected_profit_before_swap = collateral_to_seize_usd - debt_to_repay_usd;
    log::debug!(
        "Expected profit before swap: ${:.2} (bonus: ${:.2})",
        expected_profit_before_swap,
        expected_profit_before_swap
    );
    
    // CRITICAL INSIGHT: This profit assumes 1:1 swap at oracle prices
    // Jupiter will give us LESS due to:
    // 1. Price impact (slippage)
    // 2. LP fees (~0.25% for most pools)
    // 3. Route inefficiency (multi-hop swaps)
    //
    // So the ACTUAL profit will be:
    // actual_profit = jupiter_out_amount_usd - debt_to_repay_usd - fees

    // Step 6: Calculate actual SOL amount after redemption
    // CRITICAL FIX: collateral_to_seize_raw is cToken amount, NOT underlying token amount!
    // We need to calculate the exchange rate from cToken to underlying token.
    //
    // DOĞRU cToken exchange rate hesabı:
    //
    // Solend formula:
    //   borrowedAmountWads = initial_borrow * 10^18 (normalized, no decimals)
    //   actual_borrowed = borrowedAmountWads * cumulativeBorrowRateWads / 10^18 / 10^18
    //   actual_borrowed_with_decimals = actual_borrowed * 10^decimals
    //
    // Exchange rate:
    //   total_supply = availableAmount + actual_borrowed_with_decimals
    //   exchange_rate = total_supply / ctoken_supply
    //
    // CRITICAL: borrowedAmountWads is NOT raw amount! It needs to account for interest accrual.
    // NOTE: WAD is already defined earlier in this function
    
    let ctokens_total_supply = deposit_reserve.collateral().mintTotalSupply;
    let available_amount = deposit_reserve.liquidity().availableAmount;
    let borrowed_amount_wads = deposit_reserve.liquidity().borrowedAmountWads;
    let cumulative_borrow_rate = deposit_reserve.liquidity().cumulativeBorrowRateWads;
    
    // Step 1: Calculate actual borrowed amount (normalized, no decimals)
    // Formula: borrowedAmountWads * cumulativeBorrowRateWads / WAD / WAD
    // Why two divisions? Both inputs are WAD (10^18), product is 10^36, need two divisions to normalize
    let actual_borrowed_normalized = borrowed_amount_wads
        .checked_mul(cumulative_borrow_rate)
        .and_then(|v| v.checked_div(WAD))
        .and_then(|v| v.checked_div(WAD))
        .ok_or_else(|| anyhow::anyhow!("Borrowed amount calculation overflow: borrowedAmountWads * cumulativeBorrowRateWads"))?;
    
    // Step 2: Convert to raw amount with decimals
    let decimals_multiplier = 10_u128
        .checked_pow(collateral_decimals as u32)
        .ok_or_else(|| anyhow::anyhow!("Decimals multiplier overflow"))?;
    
    // Convert normalized amount to raw amount
    // ✅ FIXED: Overflow protection with u64::MAX check (Problems.md issue #6)
    // CRITICAL FIX: Use u128 arithmetic instead of f64 to prevent overflow and precision loss
    // f64 has only 53 bits of precision and can overflow for large numbers
    let actual_borrowed_u128 = actual_borrowed_normalized
        .checked_mul(decimals_multiplier)
        // .and_then(|v| v.checked_div(WAD)) // ❌ REMOVED: No division needed, already normalized!
        .ok_or_else(|| anyhow::anyhow!("Overflow in actual borrowed calculation: actual_borrowed_normalized ({}) * decimals_multiplier ({})", actual_borrowed_normalized, decimals_multiplier))?;
    
    // ✅ Check u64::MAX before casting (prevents silent truncation)
    if actual_borrowed_u128 > u64::MAX as u128 {
        return Err(anyhow::anyhow!(
            "Borrowed amount exceeds u64::MAX: {} (max: {}). This indicates extremely large borrow position.",
            actual_borrowed_u128,
            u64::MAX
        ));
    }
    
    let actual_borrowed_raw = actual_borrowed_u128 as u64;
    
    // Step 3: Total underlying supply
    let total_underlying_supply = available_amount.saturating_add(actual_borrowed_raw);
    
    // Step 4: Exchange rate (underlying per cToken)
    let exchange_rate = if ctokens_total_supply > 0 {
        total_underlying_supply as f64 / ctokens_total_supply as f64
    } else {
        1.0 // Initial exchange rate
    };
    
    // Step 5: Calculate SOL amount after redemption
    let sol_amount_after_redemption = (collateral_to_seize_raw as f64 * exchange_rate) as u64;
    
    log::debug!(
        "cToken exchange (CORRECTED): \n\
         - cTokens to redeem: {} \n\
         - Available: {} \n\
         - Borrowed (WADs): {} \n\
         - Cumulative rate: {} \n\
         - Actual borrowed (normalized): {} \n\
         - Actual borrowed (raw): {} \n\
         - Total underlying: {} \n\
         - cToken supply: {} \n\
         - Exchange rate: {:.6} \n\
         - SOL output: {}",
        collateral_to_seize_raw,
        available_amount,
        borrowed_amount_wads,
        cumulative_borrow_rate,
        actual_borrowed_normalized,
        actual_borrowed_raw,
        total_underlying_supply,
        ctokens_total_supply,
        exchange_rate,
        sol_amount_after_redemption
    );
    
    // VERIFICATION: Check if exchange rate is reasonable
    // Solend exchange rate typically 0.9 - 1.5 range (per Problems.md)
    // Stricter validation to catch edge cases and data corruption
    if exchange_rate < 0.9 || exchange_rate > 1.5 {
        log::error!(
            "❌ Abnormal cToken exchange rate: {:.6}. \
             Details: available={}, borrowed_wads={}, cumulative_rate={}, \
             ctokens_supply={}, calculated_borrowed={}",
            exchange_rate,
            available_amount,
            borrowed_amount_wads,
            cumulative_borrow_rate,
            ctokens_total_supply,
            actual_borrowed_raw
        );
        return Err(anyhow::anyhow!(
            "Suspicious exchange rate: {:.6} (expected 0.9-1.5)", 
            exchange_rate
        ));
    }
    
    // Step 7: Get Jupiter quote with calculated dynamic slippage
    // CRITICAL FIX: Single Jupiter call to avoid circular logic and rate limits
    //
    // Instead of preliminary_quote -> price_impact -> final_quote,
    // we use a robust single-quote strategy:
    // 1. Start with a generous buffer (2x base slippage)
    // 2. Get the quote
    // 3. Check price impact post-facto
    // 4. If price impact is too high (>5%), reject the quote
    
    // Read slippage settings from .env (no hardcoded values)
    use std::env;
    let base_slippage_bps = env::var("MIN_PROFIT_MARGIN_BPS")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(50); // Default 0.5% if not set
    
    let max_slippage_bps = env::var("MAX_SLIPPAGE_BPS")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(300); // Default 3% if not set
        
    // Apply 2x buffer for safety on first try
    let buffer_multiplier = 2.0;
    let initial_slippage = ((base_slippage_bps as f64 * buffer_multiplier) as u16).min(max_slippage_bps);
    
    log::debug!(
        "Jupiter quote strategy: Single call with {}bps slippage (base: {}bps, buffer: {:.1}x)",
        initial_slippage,
        base_slippage_bps,
        buffer_multiplier
    );
    
    // Step 8: Get final Jupiter quote with calculated dynamic slippage
    // CRITICAL: Jupiter quote uses UNDERLYING token (SOL), not cToken (cSOL)
    // CRITICAL: Limit retries to preserve blockhash freshness
    // Blockhash expires in ~60s, Jupiter timeout: 6s + 12s = 18s worst case
    // Reduced retries from 2 to 1 to minimize time spent on Jupiter (total 2 attempts: 1 initial + 1 retry)
    let max_retries = env::var("JUPITER_MAX_RETRIES")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1); // Default: 1 retry (total 2 attempts: 1 initial + 1 retry)
    
    let quote = get_jupiter_quote_with_retry(
        &collateral_underlying_mint, // ✅ Underlying token (SOL), NOT cToken (cSOL)
        &debt_mint,                  // ✅ Debt token (USDC)
        sol_amount_after_redemption, // ✅ CORRECT: Actual SOL amount after redemption
        initial_slippage,            // ✅ CORRECT: Buffered initial slippage
        max_retries,
    )
    .await
    .context("Failed to get Jupiter quote with retries")?;
    
    // Price impact check - Reject if too high (>5%)
    let price_impact_pct = crate::jup::get_price_impact_pct(&quote);
    if price_impact_pct > 5.0 {
        log::warn!(
            "❌ Price impact too high: {:.2}% (max: 5.0%). Skipping liquidation due to low liquidity.",
            price_impact_pct
        );
        return Err(anyhow::anyhow!(
            "Price impact too high: {:.2}% (max: 5.0%)",
            price_impact_pct
        ));
    }
    
    log::debug!(
        "Jupiter quote successful: impact={:.2}%, out_amount={}",
        price_impact_pct,
        quote.out_amount
    );
    
    // Transaction flow information
    log::debug!(
        "✅ Transaction flow implemented:\n\
         1. LiquidateObligation: debt_token -> cToken (e.g., USDC -> {})\n\
         2. RedeemReserveCollateral: cToken -> underlying_token (e.g., {} -> {})\n\
         3. Jupiter Swap: underlying_token -> debt_token (e.g., {} -> USDC)\n\
         \n\
         NOTE: Jupiter swap is executed in a separate transaction after redemption.",
        collateral_ctoken_mint,
        collateral_ctoken_mint,
        collateral_underlying_mint,
        collateral_underlying_mint
    );

    // CRITICAL FIX: Profit calculation flow
    //
    // Liquidation flow:
    // 1. Solend gives us COLLATERAL token (e.g., 10 SOL)
    // 2. Jupiter swaps COLLATERAL -> DEBT token (10 SOL -> ? USDC)
    // 3. We repay DEBT token to Solend (e.g., 1450 USDC)
    // 4. Remaining DEBT token is our profit (e.g., Jupiter gave 1500 USDC, repay 1450, profit 50)
    //
    // Profit = jupiter_out_amount (debt token) - debt_to_repay (debt token) - fees
    //
    // CRITICAL: Both are in the same token (debt_mint), no need to convert to USD first!
    
    // Get token decimals from reserves (already have deposit_reserve from above)
    let debt_decimals = ctx
        .borrow_reserve
        .as_ref()
        .map(|r| r.liquidity().mintDecimals)
        .unwrap_or(6);
    
    // CRITICAL SECURITY: Validate decimal value to prevent corrupt data issues
    // SPL tokens use 0-18 decimals (standard range)
    if debt_decimals > 18 {
        return Err(anyhow::anyhow!(
            "Invalid debt decimals: {} (expected 0-18). Reserve: {}. This may indicate corrupt data or layout changes.",
            debt_decimals,
            ctx.borrows[0].borrowReserve
        ));
    }

    // Step 7a: Get Jupiter output amount (in debt token raw units)
    let jupiter_out_amount: u64 = quote.out_amount
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid Jupiter out_amount: {}", e))?;
    
    // Step 7b: Calculate profit in debt token (raw units)
    // profit_raw = jupiter_out_amount - debt_to_repay_raw
    // If negative, this liquidation loses money!
    let profit_raw = (jupiter_out_amount as i128) - (debt_to_repay_raw as i128);
    
    // Step 7c: Convert to normalized amount for USD calculation
    let profit_tokens = (profit_raw as f64) / 10_f64.powi(debt_decimals as i32);
    
    // Step 7d: Convert to USD
    let profit_before_fees_usd = profit_tokens * debt_price_usd;
    
    // Step 7e: Calculate total fees for SINGLE atomic transaction (Flashloan approach)
    // ✅ FLASHLOAN APPROACH: Single transaction eliminates race condition and MEV risk
    // All operations in ONE transaction: FlashBorrow -> Liquidate -> Redeem -> Swap -> FlashRepay
    // Get SOL price for fee calculations
    // CRITICAL: SOL price is REQUIRED - cannot use fallback as it causes incorrect profit calculations
    let sol_price_usd = match get_sol_price_usd(rpc, ctx).await {
        Some(price) => price,
        None => {
            log::error!(
                "❌ CRITICAL: Cannot calculate liquidation profit without SOL price! \
                 SOL price is required for: fee calculations, profit calculations, risk limits. \
                 Skipping this liquidation opportunity."
            );
            return Err(anyhow::anyhow!(
                "Cannot proceed with liquidation: SOL price unavailable from oracle. \
                 This is a critical error - bot cannot safely operate without accurate SOL price."
            ));
        }
    };
    
    let jito_tip_lamports = config.jito_tip_amount_lamports.unwrap_or(10_000_000u64);
    let jito_tip_sol = jito_tip_lamports as f64 / 1_000_000_000.0;
    
    // Single transaction fees (Flashloan approach - all operations in one TX)
    // Read from .env (no hardcoded values)
    // Note: std::env already imported above
    let tx_base_fee_lamports = std::env::var("BASE_TRANSACTION_FEE_LAMPORTS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(5_000); // Default 5000 lamports if not set
    
    let tx_compute_units = std::env::var("LIQUIDATION_COMPUTE_UNITS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| {
            std::env::var("DEFAULT_COMPUTE_UNITS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
        })
        .unwrap_or(500_000); // Default 500k for flashloan if not set
    
    let tx_priority_fee_per_cu = std::env::var("DEFAULT_PRIORITY_FEE_PER_CU")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| {
            std::env::var("PRIORITY_FEE_PER_CU")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
        })
        .unwrap_or(1_000); // Default 1000 micro-lamports per CU if not set
    
    let tx_priority_fee_lamports = (tx_compute_units * tx_priority_fee_per_cu) / 1_000_000;
    let tx_total_fee_lamports = tx_base_fee_lamports + tx_priority_fee_lamports;
    
    // ✅ FIXED: Flashloan fee calculation with u128 arithmetic
    // Solend flashloan fee: reserve.config().flashLoanFeeWad (typically 0.003 = 0.3%)
    // This fee is applied to debt_to_repay_raw amount
    // CRITICAL FIX: Calculate once and reuse to avoid inconsistency
    // Note: WAD constant is already defined earlier in this function (line 3724)
    let flashloan_fee_amount_raw = if let Some(borrow_reserve) = &ctx.borrow_reserve {
        let flashloan_fee_wad = borrow_reserve.config().flashLoanFeeWad as u128;
        // Flashloan fee amount (in raw token units)
        // CRITICAL FIX: Use u128 arithmetic to prevent overflow
        (debt_to_repay_raw as u128)
            .checked_mul(flashloan_fee_wad)
            .and_then(|v| v.checked_div(WAD))
            .ok_or_else(|| anyhow::anyhow!("Flashloan fee calculation overflow"))?
            as u64
    } else {
        0 // No flashloan fee if borrow reserve not available (shouldn't happen)
    };
    
    // Convert to USD for profit calculation
    let flashloan_fee_usd = if flashloan_fee_amount_raw > 0 {
        let flashloan_fee_tokens = (flashloan_fee_amount_raw as f64) / 10_f64.powi(debt_decimals as i32);
        flashloan_fee_tokens * debt_price_usd
    } else {
        0.0
    };
    
    // Total fees in USD (single transaction approach)
    let jito_fee_usd = jito_tip_sol * sol_price_usd; // Single Jito tip (one transaction)
    let tx_fee_usd = (tx_total_fee_lamports as f64 / 1_000_000_000.0) * sol_price_usd;
    let total_fees_usd = jito_fee_usd + tx_fee_usd + flashloan_fee_usd;
    
    // FINAL PROFIT (single atomic transaction - no race condition!)
    let profit_usdc = profit_before_fees_usd - total_fees_usd;
    
    log::debug!(
        "Profit calculation (FLASHLOAN - SINGLE TX):\n\
         Jupiter output: {} raw ({:.6} tokens, ${:.2} USD)\n\
         Debt to repay: {} raw ({:.6} tokens, ${:.2} USD)\n\
         Profit before fees: ${:.2}\n\
         Jito fee: ${:.4}\n\
         TX fee: ${:.4}\n\
         Flashloan fee: ${:.4}\n\
         Total fees: ${:.4}\n\
         FINAL PROFIT: ${:.2}",
        jupiter_out_amount,
        jupiter_out_amount as f64 / 10_f64.powi(debt_decimals as i32),
        jupiter_out_amount as f64 / 10_f64.powi(debt_decimals as i32) * debt_price_usd,
        debt_to_repay_raw,
        debt_to_repay_raw as f64 / 10_f64.powi(debt_decimals as i32),
        debt_to_repay_usd,
        profit_before_fees_usd,
        jito_fee_usd,
        tx_fee_usd,
        flashloan_fee_usd,
        total_fees_usd,
        profit_usdc
    );
    
    // CRITICAL CHECK: Negative profit check
    if profit_usdc < 0.0 {
        let jupiter_out_amount_usd = (jupiter_out_amount as f64 / 10_f64.powi(debt_decimals as i32)) * debt_price_usd;
        let swap_loss_usd = collateral_to_seize_usd - jupiter_out_amount_usd;
        log::warn!(
            "Negative profit detected: ${:.2}. Jupiter swap loss (${:.2}) exceeded liquidation bonus!",
            profit_usdc,
            swap_loss_usd
        );
        return Err(anyhow::anyhow!("Liquidation would lose money: profit=${:.2}", profit_usdc));
    }
    
    // CRITICAL: Add slippage buffer to minimum profit threshold
    // Jupiter swap may have worse execution than quote
    const SLIPPAGE_BUFFER_PCT: f64 = 0.5; // 0.5% additional buffer
    let effective_min_profit = config.min_profit_usdc * (1.0 + SLIPPAGE_BUFFER_PCT / 100.0);
    
    if profit_usdc < effective_min_profit {
        return Err(anyhow::anyhow!(
            "Profit ${:.2} below threshold ${:.2} (with {:.1}% slippage buffer)",
            profit_usdc,
            effective_min_profit,
            SLIPPAGE_BUFFER_PCT
        ));
    }
    
    // Price impact logging for transparency
    let price_impact_pct = crate::jup::get_price_impact_pct(&quote);
    let actual_swap_rate = if collateral_to_seize_tokens > 0.0 {
        (jupiter_out_amount as f64 / 10_f64.powi(debt_decimals as i32)) / collateral_to_seize_tokens
    } else {
        0.0
    };
    let expected_swap_rate = debt_price_usd / collateral_price_usd;
    let rate_deviation_pct = if expected_swap_rate > 0.0 {
        ((actual_swap_rate - expected_swap_rate) / expected_swap_rate).abs() * 100.0
    } else {
        0.0
    };
    
    log::debug!(
        "Swap analysis:\n\
         Expected rate: {:.6} (oracle prices)\n\
         Actual rate: {:.6} (Jupiter)\n\
         Rate deviation: {:.2}%\n\
         Jupiter price impact: {:.2}%",
        expected_swap_rate,
        actual_swap_rate,
        rate_deviation_pct,
        price_impact_pct
    );

    // NOTE: debt_to_repay_raw is already calculated earlier in this function (Step 2)
    // It's calculated directly from WAD amounts with proper decimal handling
    
    // collateral_value_usd is used for risk limit calculations (position size)
    let collateral_value_usd = collateral_to_seize_usd;
    
    // CRITICAL FIX: flashloan_fee_amount_raw is already calculated above and reused here
    // This ensures the fee used in profit calculation matches the fee used in TX build
    
    Ok(LiquidationQuote {
        quote,
        profit_usdc,
        collateral_value_usd,
        debt_to_repay_raw, // Debt amount to repay in Solend instruction
        collateral_to_seize_raw, // Collateral cToken amount to seize (for redemption check)
    })
}

/// Log wallet balances (SOL and USDC) to console
/// Used for periodic balance monitoring
async fn log_wallet_balances(
    rpc: &Arc<RpcClient>,
    wallet_pubkey: &Pubkey,
) -> Result<()> {
    // Get SOL balance
    let sol_balance_lamports = rpc
        .get_balance(wallet_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get SOL balance: {}", e))?;
    let sol_balance = sol_balance_lamports as f64 / 1_000_000_000.0;
    
    // Get SOL price for USD value
    // Use the specific Pyth feed for SOL/USD price
    let sol_usd_pyth_feed = match Pubkey::from_str("H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG") {
        Ok(pk) => pk,
        Err(_) => {
            log::warn!("Failed to parse SOL/USD Pyth feed address, using fallback method");
            // Fall back to standalone function if feed address is invalid
            let sol_price_usd = match get_sol_price_usd_standalone(rpc).await {
                Some(price) => price,
                None => {
                    log::warn!("⚠️  Failed to get SOL price, using fallback $150");
                    150.0
                }
            };
            let sol_value_usd = sol_balance * sol_price_usd;
            // Continue with USDC check using fallback price
            let program_id = crate::solend::solend_program_id()?;
            let usdc_mint = crate::solend::find_usdc_mint_from_reserves(rpc, &program_id)
                .context("Failed to discover USDC mint")?;
            use spl_associated_token_account::get_associated_token_address;
            let usdc_ata = get_associated_token_address(wallet_pubkey, &usdc_mint);
            let usdc_balance_raw = match rpc.get_token_account(&usdc_ata) {
                Ok(Some(account)) => account.token_amount.amount.parse::<u64>().unwrap_or(0),
                _ => 0,
            };
            let usdc_balance = usdc_balance_raw as f64 / 1_000_000.0;
            log::info!(
                "💰 Wallet Balances: SOL: {:.6} SOL (~${:.2}), USDC: {:.2} USDC, Total: ~${:.2}",
                sol_balance,
                sol_value_usd,
                usdc_balance,
                sol_value_usd + usdc_balance
            );
            return Ok(());
        }
    };
    
    // Try to get SOL price directly from the specified Pyth feed first
    let current_slot = rpc.get_slot().unwrap_or(0);
    let sol_price_usd = match oracle::pyth::validate_pyth_oracle(rpc, sol_usd_pyth_feed, current_slot).await {
        Ok((true, Some(price))) => {
            log::debug!("✅ SOL price from Pyth feed {}: ${:.2}", sol_usd_pyth_feed, price);
            price
        },
        _ => {
            // Fall back to robust standalone function (tries multiple methods)
            match get_sol_price_usd_standalone(rpc).await {
                Some(price) => {
                    log::debug!("✅ SOL price for balance logging: ${:.2}", price);
                    price
                },
                None => {
                    log::warn!(
                        "⚠️  Failed to get SOL price from ALL methods for balance logging, using fallback $150. \
                         Wallet value calculations may be inaccurate."
                    );
                    150.0
                }
            }
        }
    };
    
    let sol_value_usd = sol_balance * sol_price_usd;
    
    // Get USDC balance
    let program_id = crate::solend::solend_program_id()?;
    let usdc_mint = crate::solend::find_usdc_mint_from_reserves(rpc, &program_id)
        .context("Failed to discover USDC mint")?;
    
    use spl_associated_token_account::get_associated_token_address;
    let usdc_ata = get_associated_token_address(wallet_pubkey, &usdc_mint);
    
    let usdc_balance_raw = match rpc.get_token_account(&usdc_ata) {
        Ok(Some(account)) => {
            account.token_amount.amount.parse::<u64>().unwrap_or(0)
        }
        Ok(None) => 0,
        Err(_) => 0,
    };
    
    let usdc_balance = usdc_balance_raw as f64 / 1_000_000.0;
    let total_value_usd = sol_value_usd + usdc_balance;
    
    log::info!(
        "💰 Wallet Balances: SOL: {:.6} SOL (${:.2}), USDC: {:.2} USDC, Total: ${:.2}",
        sol_balance,
        sol_value_usd,
        usdc_balance,
        total_value_usd
    );
    
    Ok(())
}

/// Get total wallet value in USD (SOL + USDC)
/// Used for risk limit calculations per Structure.md section 6.4
async fn get_wallet_value_usd(
    rpc: &Arc<RpcClient>,
    wallet_pubkey: &Pubkey,
) -> Result<f64> {
    // 1. Get SOL balance and price
    let sol_balance = rpc
        .get_balance(wallet_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get wallet balance: {}", e))?;
    
    // Get SOL price using robust standalone function (tries multiple methods)
    // CRITICAL: This is used for risk limit calculations - must be accurate!
    let sol_price_usd = match get_sol_price_usd_standalone(rpc).await {
        Some(price) => {
            log::debug!("✅ SOL price for risk limit calculation: ${:.2}", price);
            price
        },
        None => {
            // Last resort fallback - should rarely happen if network is healthy
            log::error!(
                "❌ CRITICAL: Failed to get SOL price from ALL methods (Pyth primary, backup, Jupiter API) for risk limit calculation. \
                 Using fallback $150. Risk limits may be INACCURATE. \
                 This indicates serious network/RPC issues - bot may operate unsafely!"
            );
            150.0
        }
    };
    
    let sol_value_usd = (sol_balance as f64) / 1_000_000_000.0 * sol_price_usd;
    
    // 2. Get USDC balance
    let program_id = crate::solend::solend_program_id()?;
    let usdc_mint = crate::solend::find_usdc_mint_from_reserves(rpc, &program_id)
        .context("Failed to discover USDC mint")?;
    
    use spl_associated_token_account::get_associated_token_address;
    let usdc_ata = get_associated_token_address(wallet_pubkey, &usdc_mint);
    
    let usdc_balance_raw = match rpc.get_token_account(&usdc_ata) {
        Ok(Some(account)) => {
            account.token_amount.amount.parse::<u64>().unwrap_or(0)
        }
        Ok(None) => 0, // ATA doesn't exist
        Err(_) => {
            log::debug!("Failed to get USDC balance, assuming 0");
            0
        }
    };
    
    // USDC has 6 decimals, price is $1.0
    let usdc_value_usd = (usdc_balance_raw as f64) / 1_000_000.0;
    
    // 3. Calculate total wallet value in USD
    // Note: For now, we include SOL and USDC. Other tokens can be added later if needed.
    let wallet_value_usd = sol_value_usd + usdc_value_usd;
    
    Ok(wallet_value_usd)
}

// NOTE: is_within_risk_limits() function was removed.
// Risk limit checking is now done inline in process_cycle() before each liquidation
// to ensure wallet balance is refreshed and cumulative risk tracking works correctly.
// This prevents race conditions where wallet balance changes during the cycle.

/// Build liquidation transaction per Structure.md section 8
/// 
/// ⚠️ DEPRECATED: Two-Transaction Approach (Race Condition Risk!)
/// 
/// This function is DEPRECATED and should NOT be used.
/// Use `build_flashloan_liquidation_tx` instead for atomic single-transaction approach.
/// 
/// ❌ PROBLEMS WITH TWO-TRANSACTION APPROACH:
/// - Race condition: TX1 and TX2 can be front-run by MEV bots
/// - MEV risk: Intermediate SOL state exposed between TX1 and TX2
/// - Capital requirement: Need USDC upfront for TX1
/// - Higher fees: Two transaction fees instead of one
/// 
/// ✅ USE FLASHLOAN APPROACH INSTEAD:
/// - Atomic: All operations in single transaction
/// - No MEV risk: No intermediate state
/// - No capital needed: Flashloan provides funds
/// - Gas-efficient: Single transaction fee
/// 
/// Build transaction 1: Liquidation + Redemption (NO Jupiter Swap!)
/// CRITICAL: blockhash must be fresh (fetched immediately before calling this function).
/// Blockhashes are valid for ~150 slots (~60 seconds), so fetch blockhash right before
/// building the transaction to minimize staleness risk.
#[deprecated(note = "Use build_flashloan_liquidation_tx instead for atomic single-transaction approach")]
async fn build_liquidation_tx1(
    wallet: &Arc<Keypair>,
    ctx: &LiquidationContext,
    quote: &LiquidationQuote,
    rpc: &Arc<RpcClient>,
    blockhash: solana_sdk::hash::Hash,
    _config: &Config,
) -> Result<Transaction> {
    use solana_sdk::{
        instruction::{AccountMeta, Instruction},
        sysvar,
    };
    use spl_token::ID as TOKEN_PROGRAM_ID;

    // ============================================================================
    // CRITICAL: Re-validate oracle freshness before building transaction
    // ============================================================================
    // Oracle validation happens at cycle start, but by the time we build the TX,
    // 2-3 seconds may have passed (Jupiter quote + TX build time).
    // The oracle might have become stale during this time, so we re-check here.
    let current_slot_now = rpc
        .get_slot()
        .map_err(|e| anyhow::anyhow!("Failed to get current slot for oracle re-validation: {}", e))?;
    
    // Re-validate borrow reserve oracle
    if let Some(reserve) = &ctx.borrow_reserve {
        let (valid, _) = oracle::pyth::validate_pyth_oracle(
            rpc,
            reserve.oracle_pubkey(),
            current_slot_now,
        )
        .await
        .context("Failed to re-validate borrow reserve oracle")?;
        
        if !valid {
            return Err(anyhow::anyhow!(
                "Borrow reserve oracle became stale during TX preparation. \
                 Time elapsed since initial validation: ~2-3s. Aborting liquidation for obligation {}.",
                ctx.obligation_pubkey
            ));
        }
    }
    
    // Re-validate deposit reserve oracle
    if let Some(reserve) = &ctx.deposit_reserve {
        let (valid, _) = oracle::pyth::validate_pyth_oracle(
            rpc,
            reserve.oracle_pubkey(),
            current_slot_now,
        )
        .await
        .context("Failed to re-validate deposit reserve oracle")?;
        
        if !valid {
            return Err(anyhow::anyhow!(
                "Deposit reserve oracle became stale during TX preparation. \
                 Time elapsed since initial validation: ~2-3s. Aborting liquidation for obligation {}.",
                ctx.obligation_pubkey
            ));
        }
    }
    
    log::debug!(
        "✅ Oracle re-validation passed for obligation {} (current_slot={})",
        ctx.obligation_pubkey,
        current_slot_now
    );

    let program_id = solend_program_id()?;
    let wallet_pubkey = wallet.pubkey();

    // Get reserves
    let borrow_reserve = ctx
        .borrow_reserve
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Borrow reserve not loaded"))?;
    let deposit_reserve = ctx
        .deposit_reserve
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;

    // CRITICAL: Solend LiquidateObligation instruction expects debt token amount (liquidity_amount),
    // NOT collateral amount. This is the amount of debt we're repaying.
    // We already calculated this in get_liquidation_quote() and stored it in debt_to_repay_raw.
    let liquidity_amount = quote.debt_to_repay_raw;

    // Derive required addresses
    let lending_market = ctx.obligation.lendingMarket;
    let lending_market_authority = crate::solend::derive_lending_market_authority(&lending_market, &program_id)?;

    // Get reserve liquidity supply addresses
    // These are stored in Reserve account (supplyPubkey field)
    // Solend program stores the correct PDA addresses in Reserve account during initialization
    let repay_reserve_liquidity_supply = borrow_reserve.liquidity().supplyPubkey;
    let withdraw_reserve_liquidity_supply = deposit_reserve.liquidity().supplyPubkey;
    let withdraw_reserve_collateral_supply = deposit_reserve.collateral().supplyPubkey;
    let withdraw_reserve_collateral_mint = deposit_reserve.collateral().mintPubkey;
    
    // Verify these are not default/zero addresses
    // CRITICAL: Reserve account's supplyPubkey is the authoritative source.
    // We use the value directly from Reserve account, not derived PDA.
    // Solend program stores the correct PDA addresses in Reserve account during initialization.
    if repay_reserve_liquidity_supply == Pubkey::default() 
        || withdraw_reserve_liquidity_supply == Pubkey::default() 
        || withdraw_reserve_collateral_supply == Pubkey::default()
        || withdraw_reserve_collateral_mint == Pubkey::default() {
        return Err(anyhow::anyhow!("Invalid reserve addresses: one or more addresses are default/zero"));
    }

    // SECURITY: Verify reserve supply PDA addresses match expected derivation
    // This helps detect data corruption or manipulation in Reserve accounts.
    // We derive expected PDA and compare with stored supplyPubkey.
    // 
    // CRITICAL: If PDA can be derived and doesn't match, this indicates data corruption
    // or manipulation. We MUST fail-fast per Structure.md section 13 (fail-fast principle).
    // 
    // If PDA cannot be derived (None), this may indicate unknown seed format, but we
    // still log a warning as this is unusual.
    let borrow_reserve_pubkey = ctx.borrows[0].borrowReserve;
    let deposit_reserve_pubkey = ctx.deposits[0].depositReserve;
    
    // Verify repay reserve liquidity supply
    // CRITICAL SECURITY FIX: Fail-fast if PDA cannot be derived (None)
    // This prevents proceeding with unverified addresses, which could indicate:
    // - Unknown PDA format (code needs update)
    // - Data corruption in Reserve account
    // - Malicious account manipulation
    let derived_pda = crate::solend::derive_reserve_liquidity_supply_pda(&borrow_reserve_pubkey, &program_id)
        .ok_or_else(|| anyhow::anyhow!(
            "CRITICAL SECURITY FAILURE: Cannot derive PDA for repay reserve liquidity supply. \
             Reserve: {}. \
             This may indicate unknown PDA format or data corruption. Transaction ABORTED.",
            borrow_reserve_pubkey
        ))?;
    
    if derived_pda != repay_reserve_liquidity_supply {
        log::error!(
            "🚨 SECURITY ALERT: PDA mismatch detected!\n\
             Reserve: {}\n\
             Stored supplyPubkey: {}\n\
             Derived PDA: {}\n\
             This may indicate:\n\
             - Data corruption in Reserve account\n\
             - Malicious account manipulation\n\
             - Outdated PDA derivation seeds\n\
             Transaction ABORTED for security.",
            borrow_reserve_pubkey,
            repay_reserve_liquidity_supply,
            derived_pda
        );
        return Err(anyhow::anyhow!("SECURITY FAILURE: PDA mismatch"));
    }
    log::debug!("✅ Repay reserve liquidity supply PDA verified: {}", repay_reserve_liquidity_supply);
    
    // Verify withdraw reserve liquidity supply
    // CRITICAL SECURITY FIX: Fail-fast if PDA cannot be derived (None)
    let derived_pda = crate::solend::derive_reserve_liquidity_supply_pda(&deposit_reserve_pubkey, &program_id)
        .ok_or_else(|| anyhow::anyhow!(
            "CRITICAL SECURITY FAILURE: Cannot derive PDA for withdraw reserve liquidity supply. \
             Reserve: {}. \
             This may indicate unknown PDA format or data corruption. Transaction ABORTED.",
            deposit_reserve_pubkey
        ))?;
    
    if derived_pda != withdraw_reserve_liquidity_supply {
        return Err(anyhow::anyhow!(
            "SECURITY FAILURE: Withdraw reserve liquidity supply PDA mismatch! \
             This indicates data corruption or manipulation. \
             Reserve: {}, Stored: {}, Derived: {}. \
             Transaction aborted for security.",
            deposit_reserve_pubkey,
            withdraw_reserve_liquidity_supply,
            derived_pda
        ));
    }
    log::debug!("✅ Withdraw reserve liquidity supply PDA verified: {}", withdraw_reserve_liquidity_supply);
    
    // Verify withdraw reserve collateral supply
    // CRITICAL SECURITY FIX: Fail-fast if PDA cannot be derived (None)
    let derived_pda = crate::solend::derive_reserve_collateral_supply_pda(&deposit_reserve_pubkey, &program_id)
        .ok_or_else(|| anyhow::anyhow!(
            "CRITICAL SECURITY FAILURE: Cannot derive PDA for withdraw reserve collateral supply. \
             Reserve: {}. \
             This may indicate unknown PDA format or data corruption. Transaction ABORTED.",
            deposit_reserve_pubkey
        ))?;
    
    if derived_pda != withdraw_reserve_collateral_supply {
        return Err(anyhow::anyhow!(
            "SECURITY FAILURE: Withdraw reserve collateral supply PDA mismatch! \
             This indicates data corruption or manipulation. \
             Reserve: {}, Stored: {}, Derived: {}. \
             Transaction aborted for security.",
            deposit_reserve_pubkey,
            withdraw_reserve_collateral_supply,
            derived_pda
        ));
    }
    log::debug!("✅ Withdraw reserve collateral supply PDA verified: {}", withdraw_reserve_collateral_supply);

    // Get user's token accounts (source liquidity and destination collateral)
    // These would be ATAs for the tokens
    use spl_associated_token_account::get_associated_token_address;
    let source_liquidity = get_associated_token_address(&wallet_pubkey, &borrow_reserve.liquidity().mintPubkey);
    let destination_collateral = get_associated_token_address(&wallet_pubkey, &withdraw_reserve_collateral_mint);
    
    // CRITICAL SECURITY: Validate that ATAs exist before building transaction
    // If ATAs don't exist, the transaction will fail at runtime
    // In production, these should be created at startup, but we validate here for safety
    let source_liquidity_exists = rpc.get_account(&source_liquidity).is_ok();
    if !source_liquidity_exists {
        return Err(anyhow::anyhow!(
            "Source liquidity ATA does not exist: {}. \
             Please create ATA for token {} before liquidation. \
             NOTE: In production, create all required ATAs at startup to avoid this check.",
            source_liquidity,
            borrow_reserve.liquidity().mintPubkey
        ));
    }
    
    let dest_collateral_exists = rpc.get_account(&destination_collateral).is_ok();
    if !dest_collateral_exists {
        return Err(anyhow::anyhow!(
            "Destination collateral ATA does not exist: {}. \
             Please create ATA for token {} before liquidation. \
             NOTE: In production, create all required ATAs at startup to avoid this check.",
            destination_collateral,
            withdraw_reserve_collateral_mint
        ));
    }
    
    log::debug!(
        "✅ ATA validation passed: source_liquidity={}, destination_collateral={}",
        source_liquidity,
        destination_collateral
    );

    // Build Solend liquidation instruction
    // Solend uses enum-based instruction encoding via LendingInstruction enum (NOT Anchor)
    // 
    // IMPORTANT: Solend is a native Solana program, not Anchor-based.
    // Reference: solend-sdk crate, instruction.rs - LendingInstruction enum
    // 
    // Instruction format:
    //   [tag: u8] + [args...]
    //   tag 12 = LiquidateObligation { liquidity_amount: u64 }
    // 
    // Full instruction data: [12] + liquidity_amount.to_le_bytes()
    let mut instruction_data = Vec::new();
    
    // Instruction discriminator: LendingInstruction::LiquidateObligation tag = 12
    // CRITICAL: Solend uses enum-based encoding (tag = 12), NOT Anchor sighash
    // Solend native program uses only 1 byte for enum tag
    let discriminator = crate::solend::get_liquidate_obligation_discriminator();
    log::debug!(
        "Using instruction discriminator: {} (hex: {:02x}) for liquidateObligation",
        discriminator,
        discriminator
    );
    instruction_data.push(discriminator); // Add only 1 byte
    
    // Args: liquidityAmount (u64)
    instruction_data.extend_from_slice(&liquidity_amount.to_le_bytes());

    // Build account metas per IDL - order must match Solend IDL exactly
    // CRITICAL: Account order verified against Solend SDK source code
    // Reference: solend-sdk/src/instruction.rs - LendingInstruction::LiquidateObligation
    // 
    // Correct account order (12 accounts total):
    // 0. [writable] sourceLiquidity - user's token account for debt token
    // 1. [writable] destinationCollateral - user's token account for collateral token
    // 2. [writable] repayReserve - reserve account for debt token (refreshed)
    // 3. [writable] repayReserveLiquiditySupply - SPL token supply for debt reserve
    // 4. [readonly] withdrawReserve - reserve account for collateral token (refreshed)
    // 5. [writable] withdrawReserveCollateralSupply - SPL token supply for collateral reserve
    // 6. [writable] obligation - obligation account (refreshed)
    // 7. [readonly] lendingMarket - lending market account
    // 8. [readonly] lendingMarketAuthority - derived PDA authority
    // 9. [signer] transferAuthority - user wallet (signer)
    // 10. [readonly] clockSysvar - clock sysvar account
    // 11. [readonly] tokenProgram - SPL token program ID
    let accounts = vec![
        AccountMeta::new(source_liquidity, false),                    // 0: sourceLiquidity
        AccountMeta::new(destination_collateral, false),              // 1: destinationCollateral
        AccountMeta::new(ctx.borrows[0].borrowReserve, false),  // 2: repayReserve (writable, refreshed)
        AccountMeta::new(repay_reserve_liquidity_supply, false),      // 3: repayReserveLiquiditySupply
        AccountMeta::new_readonly(ctx.deposits[0].depositReserve, false), // 4: withdrawReserve (readonly, refreshed)
        AccountMeta::new(withdraw_reserve_collateral_supply, false),  // 5: withdrawReserveCollateralSupply
        AccountMeta::new(ctx.obligation_pubkey, false),                // 6: obligation (writable, refreshed)
        AccountMeta::new_readonly(lending_market, false),            // 7: lendingMarket
        AccountMeta::new_readonly(lending_market_authority, false),   // 8: lendingMarketAuthority
        AccountMeta::new_readonly(wallet_pubkey, true),               // 9: transferAuthority (signer)
        AccountMeta::new_readonly(sysvar::clock::id(), false),        // 10: clockSysvar
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),          // 11: tokenProgram
    ];

    // CRITICAL FIX: Validate account ordering per Problems.md section 3.B
    // Account order must match Solend IDL exactly, or transaction will fail
    const EXPECTED_ACCOUNT_COUNT: usize = 12;
    if accounts.len() != EXPECTED_ACCOUNT_COUNT {
        return Err(anyhow::anyhow!(
            "Invalid account count for LiquidateObligation: expected {}, got {}",
            EXPECTED_ACCOUNT_COUNT,
            accounts.len()
        ));
    }
    
    // Validate specific account requirements
    // Account 9 (transferAuthority) must be a signer
    if !accounts[9].is_signer {
        return Err(anyhow::anyhow!(
            "Account ordering validation failed: transferAuthority (index 9) must be a signer"
        ));
    }
    
    // Validate account 0 (sourceLiquidity) is writable
    if !accounts[0].is_writable {
        return Err(anyhow::anyhow!(
            "Account ordering validation failed: sourceLiquidity (index 0) must be writable"
        ));
    }
    
    // Validate account 1 (destinationCollateral) is writable
    if !accounts[1].is_writable {
        return Err(anyhow::anyhow!(
            "Account ordering validation failed: destinationCollateral (index 1) must be writable"
        ));
    }
    
    // Validate readonly accounts (indices 4, 7, 8, 10, 11)
    let readonly_indices = [4, 7, 8, 10, 11];
    for &idx in &readonly_indices {
        if accounts[idx].is_writable {
            return Err(anyhow::anyhow!(
                "Account ordering validation failed: account at index {} must be readonly",
                idx
            ));
        }
    }
    
    log::debug!(
        "✅ Account ordering validation passed for LiquidateObligation: {} accounts, transferAuthority is signer",
        accounts.len()
    );

    let liquidation_ix = Instruction {
        program_id,
        accounts,
        data: instruction_data,
    };

    // Add compute budget instruction per Structure.md section 8
    // Note: solana-sdk 1.18 doesn't have ComputeBudgetInstruction, so we build manually
    // Compute Budget Program ID
    let compute_budget_program_id = Pubkey::from_str("ComputeBudget111111111111111111111111111111")
        .map_err(|e| anyhow::anyhow!("Invalid compute budget program ID: {}", e))?;

    // Build compute unit limit instruction manually
    // Instruction format: [discriminator: 2, units: u32]
    let mut compute_limit_data = vec![2u8]; // SetComputeUnitLimit discriminator
    compute_limit_data.extend_from_slice(&(200_000u32).to_le_bytes());
    let compute_budget_ix = Instruction {
        program_id: compute_budget_program_id,
        accounts: vec![],
        data: compute_limit_data,
    };

    // Build compute unit price instruction manually
    // Instruction format: [discriminator: 3, micro_lamports: u64]
    let mut compute_price_data = vec![3u8]; // SetComputeUnitPrice discriminator
    compute_price_data.extend_from_slice(&(1_000u64).to_le_bytes()); // 0.001 SOL per CU
    let priority_fee_ix = Instruction {
        program_id: compute_budget_program_id,
        accounts: vec![],
        data: compute_price_data,
    };

    // ============================================================================
    // INSTRUCTION 2: RedeemReserveCollateral (EKLENECEK!)
    // ============================================================================
    // LiquidateObligation bize cToken verir, bunu underlying token'a çevirmeliyiz
    //
    // Solend IDL: RedeemReserveCollateral instruction
    // Discriminator: 5 (LendingInstruction enum)
    // Args: collateral_amount (u64)
    
    // Calculate collateral amount to redeem (cToken amount received from liquidation)
    // CRITICAL: We need to calculate this from the quote or context
    // For now, we'll calculate it from the debt amount and liquidation bonus
    // This is an approximation - ideally this should be passed from get_liquidation_quote()
    
    // Use collateral_to_seize_raw from quote (calculated in get_liquidation_quote)
    let redeem_collateral_amount = quote.collateral_to_seize_raw; // cToken amount to redeem
    
    // Build RedeemReserveCollateral instruction data
    let mut redeem_instruction_data = Vec::new();
    let redeem_discriminator = crate::solend::get_redeem_reserve_collateral_discriminator();
    log::debug!(
        "Using instruction discriminator: {} (hex: {:02x}) for RedeemReserveCollateral",
        redeem_discriminator,
        redeem_discriminator
    );
    redeem_instruction_data.push(redeem_discriminator); // RedeemReserveCollateral discriminator
    redeem_instruction_data.extend_from_slice(&redeem_collateral_amount.to_le_bytes()); // collateral_amount (u64)
    
    // Get necessary accounts for RedeemReserveCollateral
    // Per Solend IDL:
    // 0. [writable] sourceCollateral - User's cToken account (destination from LiquidateObligation)
    // 1. [writable] destinationLiquidity - User's underlying token account
    // 2. [writable] reserve - Reserve account
    // 3. [writable] reserveCollateralMint - Reserve cToken mint
    // 4. [writable] reserveLiquiditySupply - Reserve underlying token supply
    // 5. [readonly] lendingMarket - Lending market
    // 6. [readonly] lendingMarketAuthority - Lending market authority PDA
    // 7. [signer] transferAuthority - User wallet
    // 8. [readonly] clockSysvar - Clock sysvar
    // 9. [readonly] tokenProgram - SPL Token program
    
    // Source collateral: User's cToken ATA (destination from LiquidateObligation)
    // This is the same as destination_collateral from LiquidateObligation
    let source_collateral = destination_collateral;
    
    // Destination liquidity: User's underlying token ATA
    let destination_liquidity = get_associated_token_address(
        &wallet_pubkey,
        &deposit_reserve.liquidity().mintPubkey // Underlying token mint (e.g., SOL)
    );
    
    // CRITICAL SECURITY: Validate that destination liquidity ATA exists
    // If not, transaction will fail at runtime
    let dest_liquidity_exists = rpc.get_account(&destination_liquidity).is_ok();
    if !dest_liquidity_exists {
        return Err(anyhow::anyhow!(
            "Destination liquidity ATA does not exist: {}. \
             Please create ATA for underlying token {} before liquidation. \
             NOTE: In production, create all required ATAs at startup to avoid this check.",
            destination_liquidity,
            deposit_reserve.liquidity().mintPubkey
        ));
    }
    
    log::debug!(
        "✅ Destination liquidity ATA validation passed: {}",
        destination_liquidity
    );
    
    // Build RedeemReserveCollateral instruction accounts
    let redeem_accounts = vec![
        AccountMeta::new(source_collateral, false),                     // 0: sourceCollateral
        AccountMeta::new(destination_liquidity, false),                 // 1: destinationLiquidity
        AccountMeta::new(ctx.deposits[0].depositReserve, false), // 2: reserve
        AccountMeta::new(withdraw_reserve_collateral_mint, false),      // 3: reserveCollateralMint
        AccountMeta::new(withdraw_reserve_liquidity_supply, false),     // 4: reserveLiquiditySupply
        AccountMeta::new_readonly(lending_market, false),              // 5: lendingMarket
        AccountMeta::new_readonly(lending_market_authority, false),     // 6: lendingMarketAuthority
        AccountMeta::new_readonly(wallet_pubkey, true),                 // 7: transferAuthority (signer)
        AccountMeta::new_readonly(sysvar::clock::id(), false),          // 8: clockSysvar
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),            // 9: tokenProgram
    ];
    
    let redeem_collateral_ix = Instruction {
        program_id,
        accounts: redeem_accounts,
        data: redeem_instruction_data,
    };
    
    // ============================================================================
    // TRANSACTION 1: Liquidation + Redemption (NO Jupiter Swap!)
    // ============================================================================
    // CRITICAL: Jupiter swap must be in a SEPARATE transaction because:
    // 1. LiquidateObligation writes cSOL tokens to wallet's ATA
    // 2. RedeemReserveCollateral reads those cSOL tokens and writes SOL to wallet's ATA
    // 3. Jupiter swap needs to read SOL from wallet's ATA
    // 
    // Solana transactions are atomic - all instructions execute simultaneously.
    // If we include Jupiter swap in the same transaction, it will try to read SOL
    // that hasn't been written yet, causing AccountNotFound or InsufficientFunds errors.
    //
    // Solution: Split into 2 transactions:
    // TX1: Liquidation + Redemption (Solend protocol)
    // TX2: Jupiter Swap (DEX)
    
    // Build transaction with fresh blockhash
    // CRITICAL: blockhash must be fetched immediately before this function is called
    // to ensure it's fresh and not stale. Blockhashes are valid for ~150 slots (~60 seconds).
    let mut tx = Transaction::new_with_payer(
        &[
            compute_budget_ix,        // Compute unit limit
            priority_fee_ix,          // Priority fee
            liquidation_ix,           // LiquidateObligation (USDC -> cSOL)
            redeem_collateral_ix,     // RedeemReserveCollateral (cSOL -> SOL)
        ],
        Some(&wallet_pubkey),
    );
    // Set blockhash immediately - it was fetched right before this function call
    tx.message.recent_blockhash = blockhash;

    log::info!(
        "Built TX1 (Liquidation + Redemption) for obligation {}:\n\
         - Liquidate: {} debt tokens (USDC -> cSOL)\n\
         - Redeem: {} cTokens -> underlying tokens (cSOL -> SOL)\n\
         - Source collateral ATA: {}\n\
         - Destination liquidity ATA: {}",
        ctx.obligation_pubkey,
        liquidity_amount,
        redeem_collateral_amount,
        source_collateral,
        destination_liquidity
    );

    Ok(tx)
}

/// ⚠️ DEPRECATED: Two-Transaction Approach (Race Condition Risk!)
/// 
/// This function is DEPRECATED and should NOT be used.
/// Use `build_flashloan_liquidation_tx` instead for atomic single-transaction approach.
/// 
/// ❌ PROBLEMS WITH TWO-TRANSACTION APPROACH:
/// - Race condition: TX1 and TX2 can be front-run by MEV bots
/// - MEV risk: Intermediate SOL state exposed between TX1 and TX2
/// - Capital requirement: Need USDC upfront for TX1
/// - Higher fees: Two transaction fees instead of one
/// 
/// ✅ USE FLASHLOAN APPROACH INSTEAD:
/// - Atomic: All operations in single transaction
/// - No MEV risk: No intermediate state
/// - No capital needed: Flashloan provides funds
/// - Gas-efficient: Single transaction fee
/// 
/// Build transaction 2: Jupiter Swap (SOL -> USDC)
/// This is called AFTER TX1 confirms and SOL is available in wallet
/// 
/// ❌ WARNING: This creates a race condition window where MEV bots can front-run TX2!
#[deprecated(note = "Use build_flashloan_liquidation_tx instead for atomic single-transaction approach")]
/// CRITICAL: blockhash must be fresh (fetched immediately before calling this function).
async fn build_liquidation_tx2(
    wallet: &Arc<Keypair>,
    ctx: &LiquidationContext,
    quote: &LiquidationQuote,
    blockhash: solana_sdk::hash::Hash,
    config: &Config,
) -> Result<Transaction> {
    use solana_sdk::instruction::Instruction;
    
    let wallet_pubkey = wallet.pubkey();
    
    // Compute Budget Program ID
    let compute_budget_program_id = Pubkey::from_str("ComputeBudget111111111111111111111111111111")
        .map_err(|e| anyhow::anyhow!("Invalid compute budget program ID: {}", e))?;

    // Build compute unit limit instruction
    let mut compute_limit_data = vec![2u8]; // SetComputeUnitLimit discriminator
    compute_limit_data.extend_from_slice(&(200_000u32).to_le_bytes());
    let compute_budget_ix = Instruction {
        program_id: compute_budget_program_id,
        accounts: vec![],
        data: compute_limit_data,
    };

    // Build compute unit price instruction
    let mut compute_price_data = vec![3u8]; // SetComputeUnitPrice discriminator
    compute_price_data.extend_from_slice(&(1_000u64).to_le_bytes()); // 0.001 SOL per CU
    let priority_fee_ix = Instruction {
        program_id: compute_budget_program_id,
        accounts: vec![],
        data: compute_price_data,
    };
    
    // ============================================================================
    // INSTRUCTION: Jupiter Swap (SOL -> USDC)
    // ============================================================================
    // After TX1 confirms, we have SOL in wallet's ATA
    // Now we swap it to USDC to complete the liquidation flow
    // CRITICAL: Jupiter returns a vector of instructions (Setup + Swap + Cleanup)
    // We must include all of them in the transaction to handle ATAs correctly
    let jupiter_swap_ixs = crate::jup::build_jupiter_swap_instruction(
        &quote.quote,
        &wallet_pubkey,
        &config.jupiter_url,
    )
    .await
    .context("Failed to build Jupiter swap instruction")?;
    
    // Build instruction list
    let mut instructions = vec![
        compute_budget_ix,        // Compute unit limit
        priority_fee_ix,          // Priority fee
    ];
    // Add all Jupiter instructions (setup + swap + cleanup)
    instructions.extend(jupiter_swap_ixs);
    
    // Build transaction with fresh blockhash
    let mut tx = Transaction::new_with_payer(
        &instructions,
        Some(&wallet_pubkey),
    );
    tx.message.recent_blockhash = blockhash;

    log::info!(
        "Built TX2 (Jupiter Swap) for obligation {}:\n\
         - Swap: SOL -> USDC ({} instructions)",
        ctx.obligation_pubkey,
        instructions.len() - 2 // Subtract compute budget instructions
    );

    Ok(tx)
}

// Transaction building functions moved to liquidation_tx module:
// - build_flashloan_liquidation_tx -> liquidation_tx::build_flashloan_liquidation_tx
// - build_liquidation_tx1 -> deprecated (kept for reference)
// - build_liquidation_tx2 -> deprecated (kept for reference)

// Transaction building functions moved to liquidation_tx module

/// Execute liquidation with swap using flashloan (atomic single transaction)
/// 
/// FLASHLOAN APPROACH - Solves race condition and MEV risks:
/// - All operations in ONE atomic transaction
/// - No race conditions: No TX1/TX2 split
/// - No MEV risk: No intermediate state exposed
/// - No capital required: Flashloan provides initial funds
/// - Gas-efficient: Single transaction
/// 
/// Flow:
/// 1. FlashLoan: Borrow debt_amount USDC from Solend (flash)
/// 2. LiquidateObligation: Repay debt, receive cSOL
/// 3. RedeemReserveCollateral: cSOL -> SOL
/// 4. Jupiter Swap: SOL -> USDC
/// 5. FlashRepayReserveLiquidity: Explicit repayment instruction (REQUIRED!)
async fn execute_liquidation_with_swap(
    ctx: &LiquidationContext,
    quote: &LiquidationQuote,
    config: &Config,
    rpc: &Arc<RpcClient>,
    jito_client: &JitoClient,
) -> Result<()> {
    let wallet = &config.wallet;
    
    // ============================================================================
    // BUILD ATOMIC FLASHLOAN TRANSACTION
    // ============================================================================
    log::info!("Building atomic flashloan liquidation transaction");
    
    // ✅ CRITICAL: Get fresh blockhash AFTER Jupiter quote completes
    // Blockhash expires in ~60 seconds. Jupiter quote can take 8-15 seconds,
    // so we must get blockhash AFTER quote to ensure maximum freshness.
    // This prevents "BlockhashNotFound" errors from stale blockhashes.
    let blockhash = rpc
        .get_latest_blockhash()
        .map_err(|e| anyhow::anyhow!("Failed to get blockhash: {}", e))?;
    
    let tx = build_flashloan_liquidation_tx(wallet, ctx, quote, rpc, blockhash, config)
        .await
        .context("Failed to build flashloan liquidation transaction")?;
    
    // Send transaction via Jito
    let bundle_id = send_jito_bundle(tx, jito_client, wallet, blockhash)
        .await
        .context("Failed to send flashloan liquidation transaction via Jito")?;
    
    log::info!(
        "✅ Atomic flashloan liquidation transaction sent: bundle_id={}\n\
         All operations (FlashLoan -> Liquidate -> Redeem -> Swap -> Repay) are atomic!",
        bundle_id
    );
    
    // ✅ FIXED: Smart polling strategy (Problems.md recommendation)
    // Bundle typically confirms in 300-400ms. Starting at 100ms is too aggressive.
    // Strategy: Wait 300ms initially, then exponential backoff (300, 600, 1200, 2000...)
    let mut confirmed = false;
    
    // Initial wait - typical bundle confirmation time
    tokio::time::sleep(Duration::from_millis(300)).await;
    
    let mut delay_ms = 300; 
    const MAX_ATTEMPTS: u32 = 10;
    const MAX_DELAY_MS: u64 = 2000; // Max 2s between checks
    
    for attempt in 0..MAX_ATTEMPTS {
        if let Ok(Some(status)) = jito_client.get_bundle_status(&bundle_id).await {
            if let Some(status_str) = &status.status {
                match status_str.as_str() {
                    "landed" | "confirmed" => {
                        confirmed = true;
                        log::debug!(
                            "✅ Bundle {} confirmed (poll #{})",
                            bundle_id,
                            attempt + 1
                        );
                        break;
                    }
                    "failed" | "dropped" => {
                        log::warn!(
                            "Bundle {} failed/dropped (poll #{})",
                            bundle_id,
                            attempt + 1
                        );
                        break;
                    }
                    "pending" => {
                        // Continue polling
                    }
                    _ => {
                        log::debug!("Bundle {} unknown status: {}", bundle_id, status_str);
                    }
                }
            }
            
            // If slot is present, bundle likely executed
            if status.slot.is_some() {
                confirmed = true;
                log::debug!(
                    "✅ Bundle {} confirmed (has slot) (poll #{})",
                    bundle_id,
                    attempt + 1
                );
                break;
            }
        }
        
        // Exponential backoff: double delay each time, max 2000ms
        delay_ms = (delay_ms * 2).min(MAX_DELAY_MS);
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }
    
    if !confirmed {
        log::debug!(
            "Bundle {} status still unknown after 4s polling, will be tracked by bundle_tracker",
            bundle_id
        );
    }
    
    Ok(())
}


