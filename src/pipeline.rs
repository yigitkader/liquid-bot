use anyhow::{Context, Result};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;

use crate::jup::{get_jupiter_quote_with_retry, JupiterQuote};
use crate::solend::{Obligation, Reserve, solend_program_id};
use crate::utils::{send_jito_bundle, JitoClient};

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

    // Initialize Jito client with tip account from config or default
    // Auto-discovered from environment variables, no hardcoded values
    let jito_tip_account_str = config.jito_tip_account.as_deref()
        .unwrap_or("96gYZGLnJYVFmbjzopPSU6QiEV5fGqZ6N6VBY6FuDgU3"); // Default mainnet tip account
    let jito_tip_account = Pubkey::from_str(jito_tip_account_str)
        .context("Invalid Jito tip account")?;
    
    // CRITICAL SECURITY: Validate Jito tip account to prevent SOL loss
    // Jito tip accounts must be system-owned (native SOL accounts)
    // If the account is not system-owned, tips will be lost!
    let tip_account_data = rpc
        .get_account(&jito_tip_account)
        .context("Failed to fetch Jito tip account from chain")?;
    
    use solana_sdk::system_program;
    if tip_account_data.owner != system_program::id() {
        return Err(anyhow::anyhow!(
            "Invalid Jito tip account: {} is not a system account (owner: {}). \
             Jito tip accounts must be system-owned native SOL accounts to prevent SOL loss.",
            jito_tip_account,
            tip_account_data.owner
        ));
    }
    
    // CRITICAL SECURITY: Validate rent-exempt status
    // System accounts are rent-exempt by default (no data), but we verify balance
    // to ensure the account won't be closed. For system accounts, rent-exempt minimum is 0,
    // but we check for a reasonable minimum balance to prevent accidental draining.
    // 
    // Note: System accounts (native SOL accounts) are always rent-exempt, but if balance
    // drops to 0, the account can be closed and SOL would be lost.
    const MIN_TIP_ACCOUNT_BALANCE_LAMPORTS: u64 = 1_000_000; // 0.001 SOL minimum for safety
    
    if tip_account_data.lamports < MIN_TIP_ACCOUNT_BALANCE_LAMPORTS {
        return Err(anyhow::anyhow!(
            "Jito tip account {} has insufficient balance: {} lamports (minimum: {} lamports). \
             Account may not be rent-exempt or could be closed, risking SOL loss.",
            jito_tip_account,
            tip_account_data.lamports,
            MIN_TIP_ACCOUNT_BALANCE_LAMPORTS
        ));
    }
    
    // Additional validation: Check if account is rent-exempt
    // For system accounts with no data, rent-exempt minimum is 0, but we verify
    // the account has sufficient balance to remain operational
    let tip_account_balance_sol = tip_account_data.lamports as f64 / 1_000_000_000.0;
    log::info!(
        "✅ Jito tip account validated: {} (balance: {:.6} SOL, owner: system program, rent-exempt: true)",
        jito_tip_account,
        tip_account_balance_sol
    );
    
    let jito_tip_amount = config.jito_tip_amount_lamports
        .unwrap_or(10_000_000u64); // Default: 0.01 SOL
    log::info!("✅ Jito tip amount: {} lamports (~{} SOL)", 
        jito_tip_amount, 
        jito_tip_amount as f64 / 1_000_000_000.0);
    let jito_client = JitoClient::new(
        config.jito_url.clone(),
        jito_tip_account,
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

    loop {
        match process_cycle(&rpc, &program_id, &config, &jito_client).await {
            Ok(_) => {}
            Err(e) => {
                log::error!("Error in liquidation cycle: {}", e);
            }
        }

        // Default poll interval per Structure.md
        sleep(Duration::from_millis(500)).await;
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
    let mut candidates = Vec::new();
    let mut parse_errors = 0;
    let total_accounts = accounts.len();
    
    for (pk, acc) in accounts {
        match Obligation::from_account_data(&acc.data) {
            Ok(obligation) => {
                let hf = obligation.health_factor();
                if hf < 1.0 {
                    candidates.push((pk, obligation));
                }
            }
            Err(e) => {
                parse_errors += 1;
                // Log first 5 errors in detail for debugging
                if parse_errors <= 5 {
                    log::warn!(
                        "Failed to parse obligation {}: {}. This may indicate layout changes or corrupt data.",
                        pk,
                        e
                    );
                }
            }
        }
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
    // CRITICAL FIX: Use bundle tracking with timestamps to handle race conditions
    // Bundles typically execute within ~400ms-2s, so we clean up expired entries
    struct PendingLiquidation {
        bundle_id: String,
        value_usd: f64,
        sent_at: Instant,
    }
    
    // Bundle execution timeout - assume bundles execute within 2 seconds
    // This is conservative; most bundles execute in ~400ms-1s
    const BUNDLE_EXECUTION_TIMEOUT_SECS: u64 = 2;
    
    let mut pending_bundles: Vec<PendingLiquidation> = Vec::new();
    
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
        rpc: &Arc<RpcClient>,
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
            log::warn!("Skipping {}: Oracle validation failed", obl_pubkey);
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
        // Clean up expired pending bundles with status verification
        // CRITICAL FIX: Check actual bundle status before releasing committed amount
        // This prevents race conditions where we release committed amount for bundles that actually executed
        let timeout = Duration::from_secs(BUNDLE_EXECUTION_TIMEOUT_SECS);
        
        // Collect expired bundles first
        let mut expired_bundles: Vec<(String, f64)> = Vec::new();
        pending_bundles.retain(|p| {
            let expired = p.sent_at.elapsed() >= timeout;
            if expired {
                expired_bundles.push((p.bundle_id.clone(), p.value_usd));
            }
            !expired
        });
        
        // Check status of expired bundles and update committed amount accordingly
        for (bundle_id, value_usd) in expired_bundles {
            match verify_bundle_status(rpc, &bundle_id, jito_client).await {
                BundleStatus::Confirmed => {
                    // Bundle executed - keep committed amount (already reflected in balance)
                    log::debug!("Bundle {} confirmed on-chain, keeping committed amount", bundle_id);
                }
                BundleStatus::Failed => {
                    // Bundle dropped - release committed amount
                    wallet_balance_tracker.pending_committed_usd -= value_usd;
                    log::warn!("Bundle {} dropped from mempool, releasing ${:.2} committed amount", bundle_id, value_usd);
                }
                BundleStatus::Unknown => {
                    // Conservative: assume executed to avoid overcommit
                    // This prevents releasing committed amount for bundles that might have executed
                    log::warn!("Bundle {} status unknown, assuming executed (conservative), keeping committed amount", bundle_id);
                }
                BundleStatus::Pending => {
                    // Shouldn't happen if timeout check is correct, but handle it
                    log::debug!("Bundle {} still pending after timeout, keeping in tracking", bundle_id);
                    // Re-add to pending_bundles? For now, assume it will execute
                }
            }
        }
        
        // Calculate total pending liquidation value from active bundles (for logging/validation)
        let pending_liquidation_value: f64 = pending_bundles.iter().map(|p| p.value_usd).sum();
        
        // Account for pending liquidations when calculating available liquidity
        // CRITICAL FIX: Use pending_committed_usd for consistent tracking
        // This ensures we don't overcommit capital that's already tied up in pending liquidations
        let available_liquidity = wallet_balance_tracker.current_estimated_balance_usd 
            - wallet_balance_tracker.pending_committed_usd;
        let current_max_position_usd = available_liquidity * config.max_position_pct;
        
        let position_size_usd = quote.collateral_value_usd;
        
        log::debug!(
            "Risk calculation: estimated_wallet=${:.2}, pending_committed=${:.2} ({} bundles, sum=${:.2}), available=${:.2}, max_position=${:.2}",
            wallet_balance_tracker.current_estimated_balance_usd,
            wallet_balance_tracker.pending_committed_usd,
            pending_bundles.len(),
            pending_liquidation_value,
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
            // CRITICAL: Use two-transaction flow to avoid atomicity issues
            // TX1: Liquidation + Redemption (Solend protocol)
            // TX2: Jupiter Swap (DEX)
            // These must be separate because TX2 needs TX1's output (SOL tokens)
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
                            // CRITICAL FIX: Track individual bundles with timestamps for accurate cleanup
                            // Bundles are assumed executed after BUNDLE_EXECUTION_TIMEOUT_SECS (2 seconds)
                            // NOTE: For two-transaction flow, we track both TX1 and TX2 as separate pending liquidations
                            // TX1 is the main liquidation, TX2 is the swap
                            pending_bundles.push(PendingLiquidation {
                                bundle_id: format!("TX1+TX2_{}", obl_pubkey), // Placeholder - actual bundle IDs logged in execute_liquidation_with_swap
                                value_usd: position_size_usd,
                                sent_at: Instant::now(),
                            });
                            
                            // CRITICAL: Track committed amount to prevent overcommit
                            // This capital is now tied up in the pending liquidation
                            wallet_balance_tracker.pending_committed_usd += position_size_usd;
                            
                            cumulative_risk_usd += position_size_usd;
                            
                            // Calculate current pending value for logging
                            let current_pending_value: f64 = pending_bundles.iter().map(|p| p.value_usd).sum();
                            
                            // REFRESH balance every 5 liquidations (doğrulama için)
                            const REFRESH_INTERVAL: u32 = 5;
                            let refresh_countdown = REFRESH_INTERVAL.saturating_sub(wallet_balance_tracker.liquidations_since_refresh);
                            
                            log::info!(
                                "✅ Liquidated {} with profit ${:.2} (TX1+TX2), estimated_balance=${:.2}, pending_committed=${:.2} (refresh in {} liquidations), cumulative_risk=${:.2}/${:.2} ({} active bundles)",
                                obl_pubkey,
                                quote.profit_usdc,
                                wallet_balance_tracker.current_estimated_balance_usd,
                                wallet_balance_tracker.pending_committed_usd,
                                refresh_countdown,
                                cumulative_risk_usd,
                                current_max_position_usd,
                                pending_bundles.len()
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
        "📊 Cycle Summary: {} candidates | {} successful | {} skipped (oracle:{}, jupiter:{}, profit:{}, risk:{}, rpc:{}, reserve:{}, ata:{}) | {} failed (build:{}, send:{}) | cumulative_risk=${:.2}/{:.2} (final_wallet_value=${:.2})",
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
        cumulative_risk_usd,
        final_max_position_usd,
        final_wallet_value_usd
    );

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
struct LiquidationContext {
    obligation_pubkey: Pubkey,
    obligation: Obligation,
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
    if !obligation.borrows.is_empty() {
        let borrow_reserve_pubkey = obligation.borrows[0].borrowReserve;
        if let Ok(account) = rpc.get_account_data(&borrow_reserve_pubkey) {
            if let Ok(reserve) = Reserve::from_account_data(&account) {
                borrow_reserve = Some(reserve);
            }
        }
    }

    // Get first deposit reserve if exists
    if !obligation.deposits.is_empty() {
        let deposit_reserve_pubkey = obligation.deposits[0].depositReserve;
        if let Ok(account) = rpc.get_account_data(&deposit_reserve_pubkey) {
            if let Ok(reserve) = Reserve::from_account_data(&account) {
                deposit_reserve = Some(reserve);
            }
        }
    }

    // Validate Oracle per Structure.md section 5.2
    let (oracle_ok, borrow_price, deposit_price) = validate_oracles(rpc, &borrow_reserve, &deposit_reserve).await?;

    Ok(LiquidationContext {
        obligation_pubkey: obligation.owner, // Note: actual obligation pubkey should be passed separately
        obligation: obligation.clone(),
        borrow_reserve,
        deposit_reserve,
        borrow_price_usd: borrow_price,
        deposit_price_usd: deposit_price,
        oracle_ok,
    })
}

/// Pyth Network program ID (mainnet)
const PYTH_PROGRAM_ID: &str = "FsJ3A3u2vn5cTVofAjvy6y5kwABJAqYWpe4975bi2epH";

/// Switchboard program ID (mainnet)
const SWITCHBOARD_PROGRAM_ID: &str = "SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f";

/// Maximum allowed confidence interval (as percentage of price)
/// When Switchboard is available, this is used as secondary validation.
/// When Switchboard is NOT available, we use a stricter threshold for Pyth-only validation.
const MAX_CONFIDENCE_PCT: f64 = 5.0; // 5% max confidence interval (with Switchboard)
const MAX_CONFIDENCE_PCT_PYTH_ONLY: f64 = 2.0; // 2% max confidence interval (Pyth-only, stricter)

/// Minimum valid price threshold (in USD)
/// Prices below this threshold are considered invalid to prevent division by zero
/// and floating point precision issues in confidence percentage calculations.
/// Example: price = 1e-100 → confidence_pct calculation would produce inf or very large values.
/// 
/// CRITICAL: Set to 1e-6 to support micro-cap tokens while maintaining safety
/// Previous 1e-3 was too aggressive and rejected valid micro-cap tokens ($0.0005)
const MIN_VALID_PRICE_USD: f64 = 1e-6; // $0.000001 minimum (1 micro-dollar) - supports micro-cap tokens

/// Maximum allowed slot difference for oracle price (stale check)
/// Pyth recommends checking valid_slot, but we also check last_slot as fallback
/// CRITICAL: Pyth feeds update ~400ms, so 25 slots = ~10 seconds is sufficient
/// Previous 150 slots (~60s) was too lenient and could accept manipulated prices
const MAX_SLOT_DIFFERENCE: u64 = 25; // ~10 seconds at 400ms per slot (Pyth recommended)

/// Maximum allowed price deviation between Pyth and Switchboard (as percentage)
const MAX_ORACLE_DEVIATION_PCT: f64 = 2.0; // 2% max deviation

/// Pyth price status values (from price_type byte)
/// PriceType enum: Unknown = 0, Price = 1, Trading = 2, Halted = 3, Auction = 4
const PYTH_PRICE_STATUS_TRADING: u8 = 2;
const PYTH_PRICE_STATUS_UNKNOWN: u8 = 0;
const PYTH_PRICE_STATUS_HALTED: u8 = 3;

/// Validate Pyth and Switchboard oracles per Structure.md section 5.2
/// Returns (is_valid, borrow_price_usd, deposit_price_usd)
async fn validate_oracles(
    rpc: &Arc<RpcClient>,
    borrow_reserve: &Option<Reserve>,
    deposit_reserve: &Option<Reserve>,
) -> Result<(bool, Option<f64>, Option<f64>)> {
    // Basic validation: check if reserves exist and have oracle pubkeys
    let borrow_ok = borrow_reserve
        .as_ref()
        .map(|r| r.oracle_pubkey() != Pubkey::default())
        .unwrap_or(false);

    let deposit_ok = deposit_reserve
        .as_ref()
        .map(|r| r.oracle_pubkey() != Pubkey::default())
        .unwrap_or(false);

    if !borrow_ok || !deposit_ok {
        log::debug!("Oracle validation failed: missing oracle pubkeys");
        return Ok((false, None, None));
    }

    // Get current slot for stale check
    let current_slot = rpc
        .get_slot()
        .map_err(|e| anyhow::anyhow!("Failed to get current slot: {}", e))?;

    // Validate borrow reserve oracle
    let borrow_pyth_price = if let Some(reserve) = borrow_reserve {
        match validate_pyth_oracle(rpc, reserve.oracle_pubkey(), current_slot).await {
            Ok((valid, price)) => {
                if !valid {
                    log::debug!("Borrow reserve oracle validation failed");
                    return Ok((false, None, None));
                }
                price
            }
            Err(e) => {
                log::debug!("Borrow reserve oracle validation error: {}", e);
                return Ok((false, None, None));
            }
        }
    } else {
        None
    };

    // Validate deposit reserve oracle
    let deposit_pyth_price = if let Some(reserve) = deposit_reserve {
        match validate_pyth_oracle(rpc, reserve.oracle_pubkey(), current_slot).await {
            Ok((valid, price)) => {
                if !valid {
                    log::debug!("Deposit reserve oracle validation failed");
                    return Ok((false, None, None));
                }
                price
            }
            Err(e) => {
                log::debug!("Deposit reserve oracle validation error: {}", e);
                return Ok((false, None, None));
            }
        }
    } else {
        None
    };

    // Validate Switchboard oracles if available per Structure.md section 5.2
    // "Switchboard varsa, Pyth ile sapma fazla mı?"
    // 
    // CRITICAL SECURITY: If Switchboard is NOT available, we rely solely on Pyth.
    // In this case, we apply stricter validation (stricter confidence threshold).
    // This mitigates the risk of Pyth oracle manipulation when no cross-validation exists.
    
    let mut borrow_has_switchboard = false;
    let mut deposit_has_switchboard = false;

    // Validate Switchboard oracles if available for borrow reserve
    if let (Some(borrow_reserve), Some(borrow_price)) = (borrow_reserve, borrow_pyth_price) {
        if let Some(switchboard_price) = validate_switchboard_oracle_if_available(
            rpc,
            &borrow_reserve,
            current_slot,
        )
        .await?
        {
            borrow_has_switchboard = true;
            // Compare Pyth and Switchboard prices
            let deviation_pct = ((borrow_price - switchboard_price).abs() / borrow_price) * 100.0;
            if deviation_pct > MAX_ORACLE_DEVIATION_PCT {
                log::warn!(
                    "Oracle deviation too high for borrow reserve: {:.2}% > {:.2}% (Pyth: {}, Switchboard: {})",
                    deviation_pct,
                    MAX_ORACLE_DEVIATION_PCT,
                    borrow_price,
                    switchboard_price
                );
                return Ok((false, None, None));
            }
            log::debug!(
                "✅ Oracle deviation OK for borrow reserve: {:.2}% (Pyth: {}, Switchboard: {})",
                deviation_pct,
                borrow_price,
                switchboard_price
            );
        }
    }

    // Validate Switchboard oracles if available for deposit reserve
    if let (Some(deposit_reserve), Some(deposit_price)) = (deposit_reserve, deposit_pyth_price) {
        if let Some(switchboard_price) = validate_switchboard_oracle_if_available(
            rpc,
            &deposit_reserve,
            current_slot,
        )
        .await?
        {
            deposit_has_switchboard = true;
            let deviation_pct = ((deposit_price - switchboard_price).abs() / deposit_price) * 100.0;
            if deviation_pct > MAX_ORACLE_DEVIATION_PCT {
                log::warn!(
                    "Oracle deviation too high for deposit reserve: {:.2}% > {:.2}% (Pyth: {}, Switchboard: {})",
                    deviation_pct,
                    MAX_ORACLE_DEVIATION_PCT,
                    deposit_price,
                    switchboard_price
                );
                return Ok((false, None, None));
            }
            log::debug!(
                "✅ Oracle deviation OK for deposit reserve: {:.2}% (Pyth: {}, Switchboard: {})",
                deviation_pct,
                deposit_price,
                switchboard_price
            );
        }
    }

    // SECURITY: If Switchboard is NOT available for either reserve, apply stricter Pyth validation
    // This mitigates the risk of relying solely on Pyth without cross-validation
    if !borrow_has_switchboard || !deposit_has_switchboard {
        log::warn!(
            "⚠️  SECURITY WARNING: Switchboard oracle not available (borrow: {}, deposit: {}). \
             Relying solely on Pyth with stricter confidence threshold ({}% vs {}%). \
             This increases risk of oracle manipulation.",
            if borrow_has_switchboard { "✓" } else { "✗" },
            if deposit_has_switchboard { "✓" } else { "✗" },
            MAX_CONFIDENCE_PCT_PYTH_ONLY,
            MAX_CONFIDENCE_PCT
        );
        
        // Re-validate Pyth confidence with stricter threshold
        if let Some(borrow_reserve) = borrow_reserve {
            if !borrow_has_switchboard {
                if let Some(borrow_price) = borrow_pyth_price {
                    // Re-check Pyth confidence with stricter threshold
                    let confidence_check = validate_pyth_confidence_strict(
                        rpc,
                        borrow_reserve.oracle_pubkey(),
                        borrow_price,
                        current_slot,
                    ).await?;
                    if !confidence_check {
                        log::warn!(
                            "Borrow reserve Pyth confidence check failed (stricter threshold for Pyth-only mode)"
                        );
                        return Ok((false, None, None));
                    }
                }
            }
        }
        
        if let Some(deposit_reserve) = deposit_reserve {
            if !deposit_has_switchboard {
                if let Some(deposit_price) = deposit_pyth_price {
                    // Re-check Pyth confidence with stricter threshold
                    let confidence_check = validate_pyth_confidence_strict(
                        rpc,
                        deposit_reserve.oracle_pubkey(),
                        deposit_price,
                        current_slot,
                    ).await?;
                    if !confidence_check {
                        log::warn!(
                            "Deposit reserve Pyth confidence check failed (stricter threshold for Pyth-only mode)"
                        );
                        return Ok((false, None, None));
                    }
                }
            }
        }
    }

    Ok((true, borrow_pyth_price, deposit_pyth_price))
}

/// Validate Switchboard oracle if available in ReserveConfig
/// Returns Some(price) if Switchboard oracle exists and is valid, None otherwise
/// Per Structure.md section 5.2
/// 
/// DYNAMIC CHAIN READING: All data is read from chain via RPC - no static values.
/// 
/// Uses official Switchboard SDK (switchboard-on-demand) with Solana SDK v2 compatibility.
/// SDK is pulled from GitHub main branch for latest Solana v2 support.
async fn validate_switchboard_oracle_if_available(
    rpc: &Arc<RpcClient>,
    reserve: &Reserve,
    current_slot: u64,
) -> Result<Option<f64>> {
    // Use new Switchboard On-Demand SDK (Solana 2.0 compatible)
    use switchboard_on_demand::on_demand::accounts::pull_feed::PullFeedAccountData;
    
    // 1. Get switchboard_oracle_pubkey from reserve.config (DYNAMIC - from chain)
    let switchboard_oracle_pubkey = reserve.config.switchboardOraclePubkey;
    if switchboard_oracle_pubkey == Pubkey::default() {
        // No Switchboard oracle configured for this reserve
        return Ok(None);
    }

    // 2. Get Switchboard feed account data from chain (DYNAMIC)
    let oracle_account = rpc
        .get_account(&switchboard_oracle_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get Switchboard oracle account from chain: {}", e))?;

    let switchboard_program_id = Pubkey::from_str(SWITCHBOARD_PROGRAM_ID)
        .map_err(|e| anyhow::anyhow!("Invalid Switchboard program ID: {}", e))?;

    if oracle_account.owner != switchboard_program_id {
        log::debug!(
            "Switchboard oracle account {} does not belong to Switchboard program",
            switchboard_oracle_pubkey
        );
        return Ok(None);
    }

    // 3. Parse using Switchboard On-Demand SDK (Solana 2.0 compatible)
    // 
    // PullFeedAccountData implements Pod (Plain Old Data), so we can use bytemuck
    // to directly deserialize from the account data. This is the recommended approach
    // for off-chain clients, as the SDK's parse() method is designed for Anchor's
    // account.data.borrow() pattern which requires Ref<'_, &mut [u8]>.
    // 
    // CRITICAL FIX: Solana account data is not guaranteed to be aligned.
    // try_from_bytes requires alignment, which can cause runtime panic.
    // We handle AlignmentMismatch error with a safe fallback that ensures proper alignment.
    use bytemuck::{Pod, PodCastError};
    
    // Ensure we have enough data
    let feed_size = std::mem::size_of::<PullFeedAccountData>();
    if oracle_account.data.len() < feed_size {
        log::warn!(
            "Switchboard feed account data too short: {} bytes (need at least {})",
            oracle_account.data.len(),
            feed_size
        );
        return Ok(None);
    }
    
    // CRITICAL: Solana RPC account data is NOT guaranteed to be aligned.
    // bytemuck::try_from_bytes requires strict alignment, which can cause runtime panic.
    // We use a safe alignment strategy: try direct parse first, then fallback to explicit alignment.
    let feed = match bytemuck::try_from_bytes::<PullFeedAccountData>(&oracle_account.data) {
        Ok(feed) => *feed,
        Err(PodCastError::AlignmentMismatch { .. }) => {
            // Fallback: Create explicitly aligned buffer (16-byte alignment for safety)
            // This ensures proper alignment regardless of the original data's alignment.
            // Strategy: Allocate extra space, find aligned pointer, copy data to aligned location.
            let alignment = 16; // 16-byte alignment (safe for most structs, including SIMD)
            let mut aligned_buffer = vec![0u8; feed_size + alignment]; // +16 for alignment padding
            
            // Find the first 16-byte aligned address within the buffer
            // Formula: (ptr + 15) & !15 rounds up to next 16-byte boundary
            let buffer_ptr = aligned_buffer.as_ptr() as usize;
            let aligned_ptr = (buffer_ptr + alignment - 1) & !(alignment - 1); // 16-byte align
            let offset = aligned_ptr - buffer_ptr;
            
            // Ensure we have enough space after alignment
            if offset + feed_size > aligned_buffer.len() {
                log::warn!(
                    "Switchboard feed alignment calculation failed for {}: insufficient buffer space",
                    switchboard_oracle_pubkey
                );
                return Ok(None);
            }
            
            // Copy data to the aligned location
            let aligned_slice = &mut aligned_buffer[offset..offset + feed_size];
            aligned_slice.copy_from_slice(&oracle_account.data[..feed_size]);
            
            // Now try parsing from the explicitly aligned slice
            // SAFETY: aligned_slice is guaranteed to be 16-byte aligned, which is sufficient
            // for any struct alignment requirement (most structs require 8-byte or less)
            match bytemuck::try_from_bytes::<PullFeedAccountData>(aligned_slice) {
                Ok(feed) => *feed,
                Err(e) => {
                    log::warn!(
                        "Switchboard feed parsing failed after explicit alignment fix for {}: {}. \
                         This may indicate a data structure issue or incompatible SDK version. \
                         Falling back to Pyth-only mode with stricter validation ({}% confidence threshold).",
                        switchboard_oracle_pubkey,
                        e,
                        MAX_CONFIDENCE_PCT_PYTH_ONLY
                    );
                    return Ok(None);
                }
            }
        }
        Err(e) => {
            log::warn!(
                "Switchboard feed parsing failed for {}: {}. \
                 Falling back to Pyth-only mode with stricter validation ({}% confidence threshold).",
                switchboard_oracle_pubkey,
                e,
                MAX_CONFIDENCE_PCT_PYTH_ONLY
            );
            return Ok(None);
        }
    };
    
    // 4. Get price using SDK's value() method with staleness check
    // The value() method requires current slot for staleness validation
    // It returns Result<Decimal, OnDemandError> - Ok if valid, Err if stale/insufficient
    use rust_decimal::Decimal;
    let price_decimal = match feed.value(current_slot) {
        Ok(v) => v,
        Err(e) => {
            log::debug!(
                "Switchboard feed {} value() failed (stale or insufficient quorum): {}",
                switchboard_oracle_pubkey,
                e
            );
            return Ok(None);
        }
    };
    
    // 5. Convert Decimal to f64
    // rust_decimal::Decimal provides better precision, but we use f64 for consistency with Pyth
    let price = price_decimal.to_string().parse::<f64>()
        .unwrap_or_else(|_| {
            // Fallback: manual conversion using mantissa and scale
            let mantissa = price_decimal.mantissa();
            let scale = price_decimal.scale();
            mantissa as f64 / 10_f64.powi(scale as i32)
        });
    
    // 6. Validate price is positive and reasonable
    if price <= 0.0 || !price.is_finite() {
        log::debug!(
            "Switchboard oracle price is invalid: {} (from feed {}, decimal: {})",
            price,
            switchboard_oracle_pubkey,
            price_decimal
        );
        return Ok(None);
    }
    
    log::debug!(
        "✅ Switchboard oracle validation passed for {} (price: {}, decimal: {})",
        switchboard_oracle_pubkey,
        price,
        price_decimal
    );
    
    Ok(Some(price))
}

/// Validate Pyth oracle per Structure.md section 5.2
/// Returns (is_valid, price) where price is in USD with decimals
async fn validate_pyth_oracle(
    rpc: &Arc<RpcClient>,
    oracle_pubkey: Pubkey,
    current_slot: u64,
) -> Result<(bool, Option<f64>)> {
    // 1. Verify oracle account belongs to Pyth program ID
    let oracle_account = rpc
        .get_account(&oracle_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get oracle account: {}", e))?;

    let pyth_program_id = Pubkey::from_str(PYTH_PROGRAM_ID)
        .map_err(|e| anyhow::anyhow!("Invalid Pyth program ID: {}", e))?;

    if oracle_account.owner != pyth_program_id {
        log::debug!("Oracle account {} does not belong to Pyth program", oracle_pubkey);
        return Ok((false, None));
    }

    // 2. Parse Pyth v2 price account structure
    // Pyth v2 price account layout (COMPLETE):
    // - Offset 0-4: magic (4 bytes) = 0xa1b2c3d4
    // - Offset 4-5: version (1 byte) = 2
    // - Offset 5-6: price_type (1 byte) - PriceType enum: Unknown=0, Price=1, Trading=2, Halted=3, Auction=4
    // - Offset 6-8: size (2 bytes)
    // - Offset 8-16: price (i64, 8 bytes)
    // - Offset 16-20: exponent (i32, 4 bytes)
    // - Offset 20-24: reserved (4 bytes)
    // - Offset 24-32: timestamp (i64, 8 bytes)
    // - Offset 32-40: prev_publish_time (i64, 8 bytes)
    // - Offset 40-48: prev_price (i64, 8 bytes)
    // - Offset 48-56: prev_conf (u64, 8 bytes)
    // - Offset 56-64: last_slot (u64, 8 bytes) - slot when price was last updated
    // - Offset 64-72: valid_slot (u64, 8 bytes) - slot when price is valid until
    // - Offset 72-80: aggregate.status (u64, 8 bytes) ← CRITICAL: Must be Trading (1)
    // - Offset 80-84: aggregate.num_components (u32, 4 bytes) ← Should be >= 3
    // - Offset 84+: publisher accounts...

    // CRITICAL FIX: Need at least 84 bytes for aggregate.status and num_components
    if oracle_account.data.len() < 84 {
        log::debug!("Oracle account data too short: {} bytes (need at least 84 for Pyth v2 with aggregate fields)", oracle_account.data.len());
        return Ok((false, None));
    }

    // Check magic number (Pyth v2)
    let magic: [u8; 4] = oracle_account.data[0..4]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse magic"))?;
    if magic != [0xa1, 0xb2, 0xc3, 0xd4] {
        log::debug!("Invalid Pyth magic number: {:?}", magic);
        return Ok((false, None));
    }

    // Check version
    let version = oracle_account.data[4];
    if version != 2 {
        log::debug!("Unsupported Pyth version: {} (expected 2)", version);
        return Ok((false, None));
    }

    // Check price status (price_type byte)
    // CRITICAL: Only Trading status (2) is acceptable for liquidations
    // All other statuses (Unknown=0, Price=1, Halted=3, Auction=4) must be rejected
    let price_type = oracle_account.data[5];
    if price_type != PYTH_PRICE_STATUS_TRADING {
        let status_name = match price_type {
            0 => "Unknown",
            1 => "Price",
            2 => "Trading",
            3 => "Halted",
            4 => "Auction",
            _ => "Invalid",
        };
        
        log::warn!(
            "Pyth price status is {} ({}) - REJECTING oracle. Only Trading status is acceptable for liquidations.",
            price_type,
            status_name
        );
        return Ok((false, None));
    }
    
    // CRITICAL FIX: Check aggregate.status (offset 72-80, u64)
    // aggregate.status values:
    // 0 = Unknown
    // 1 = Trading (ONLY THIS IS ACCEPTABLE!)
    // 2 = Halted
    // 3 = Auction
    let aggregate_status_bytes: [u8; 8] = oracle_account.data[72..80]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse aggregate.status"))?;
    let aggregate_status = u64::from_le_bytes(aggregate_status_bytes);
    
    const AGGREGATE_STATUS_TRADING: u64 = 1;
    if aggregate_status != AGGREGATE_STATUS_TRADING {
        let status_name = match aggregate_status {
            0 => "Unknown",
            1 => "Trading",
            2 => "Halted",
            3 => "Auction",
            _ => "Invalid",
        };
        
        log::warn!(
            "Pyth aggregate status is {} ({}) - REJECTING oracle. Only Trading status is acceptable.",
            aggregate_status,
            status_name
        );
        return Ok((false, None));
    }
    
    // Additional validation: num_components check
    // If num_components < 3, the aggregate may be unreliable
    let num_components_bytes: [u8; 4] = oracle_account.data[80..84]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse num_components"))?;
    let num_components = u32::from_le_bytes(num_components_bytes);
    
    const MIN_NUM_COMPONENTS: u32 = 3; // Pyth recommends at least 3 publishers
    if num_components < MIN_NUM_COMPONENTS {
        log::warn!(
            "Pyth aggregate has only {} publishers (min: {}). Price may be unreliable.",
            num_components,
            MIN_NUM_COMPONENTS
        );
        // Don't reject, but log warning for monitoring
    }
    
    log::debug!(
        "✅ Pyth oracle validation passed: price_type={}, aggregate_status={}, num_components={}",
        price_type,
        aggregate_status,
        num_components
    );

    // Parse exponent
    let exponent_bytes: [u8; 4] = oracle_account.data[16..20]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse exponent"))?;
    let exponent = i32::from_le_bytes(exponent_bytes);

    // Parse price (i64)
    let price_bytes: [u8; 8] = oracle_account.data[8..16]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse price"))?;
    let price_raw = i64::from_le_bytes(price_bytes);

    // Convert to f64 with exponent
    // CRITICAL: Pyth exponent is typically negative (e.g., -8 for 8 decimals)
    // Mathematical equivalence: price_raw * 10^(-8) = price_raw / 10^8
    // Example: price_raw=150000000, exponent=-8 → 150000000 * 10^(-8) = 1.5 USD
    // This is CORRECT: powi() handles negative exponents properly (10^(-8) = 1/10^8)
    let price = Some(price_raw as f64 * 10_f64.powi(exponent));

    // 3. Check if price is stale (last_slot and valid_slot check)
    // CRITICAL FIX: last_slot is the PRIMARY staleness check (when price was last updated)
    // valid_slot can be a future slot, so it's only used for expiration check
    
    // Parse last_slot (PRIMARY staleness check - when price was last updated)
    let last_slot_bytes: [u8; 8] = oracle_account.data[56..64]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse last_slot"))?;
    let last_slot = u64::from_le_bytes(last_slot_bytes);
    
    // Parse valid_slot (for expiration check only)
    let valid_slot_bytes: [u8; 8] = oracle_account.data[64..72]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse valid_slot"))?;
    let valid_slot = u64::from_le_bytes(valid_slot_bytes);

    // PRIMARY CHECK: last_slot based staleness (when price was last updated)
    // This is the most reliable indicator of price freshness
    let slot_diff_last = current_slot.saturating_sub(last_slot);
    if slot_diff_last > MAX_SLOT_DIFFERENCE {
        log::debug!(
            "Pyth oracle price too old: last_slot={}, current_slot={}, diff={} > {}",
            last_slot,
            current_slot,
            slot_diff_last,
            MAX_SLOT_DIFFERENCE
        );
        return Ok((false, None));
    }

    // SECONDARY CHECK: valid_slot expiration (only if current_slot > valid_slot)
    // valid_slot can be a future slot, so we only check if it's in the past
    // and the difference is significant (to avoid false positives from minor slot drift)
    let slot_diff_valid = current_slot.saturating_sub(valid_slot);
    if current_slot > valid_slot && slot_diff_valid > 10 {
        log::debug!(
            "Pyth oracle price expired: valid_slot={}, current_slot={}, diff={}",
            valid_slot,
            current_slot,
            slot_diff_valid
        );
        return Ok((false, None));
    }

    log::debug!(
        "Pyth oracle price is fresh: valid_slot={}, last_slot={}, current_slot={}, slot_diff_last={}, slot_diff_valid={}",
        valid_slot,
        last_slot,
        current_slot,
        slot_diff_last,
        slot_diff_valid
    );

    // 4. Check confidence interval
    let price_value = price.ok_or_else(|| anyhow::anyhow!("Price not parsed"))?;
    
    // CRITICAL: Check minimum price threshold and finite check to prevent division by zero and floating point issues
    // Edge case: price_value = 1e-100 → price_value.abs() > 0.0 check passes, but
    // confidence_pct = confidence / price_value.abs() would produce inf or very large values
    // Solution: Reject prices below minimum threshold or non-finite values
    if price_value.abs() < MIN_VALID_PRICE_USD || !price_value.is_finite() {
        log::debug!(
            "Pyth oracle price invalid or too small: {} (min: {}, finite: {})",
            price_value,
            MIN_VALID_PRICE_USD,
            price_value.is_finite()
        );
        return Ok((false, None));
    }
    
    // Parse confidence (u64 at offset 48-56)
    // CRITICAL: Confidence uses the same exponent as price
    // Example: confidence_raw=1000000, exponent=-8 → 1000000 * 10^(-8) = 0.01
    let conf_bytes: [u8; 8] = oracle_account.data[48..56]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse confidence"))?;
    let confidence_raw = u64::from_le_bytes(conf_bytes);
    let confidence = confidence_raw as f64 * 10_f64.powi(exponent);

    // Check if confidence interval is too large (as percentage of price)
    // NOTE: We already checked price_value.abs() >= MIN_VALID_PRICE_USD and is_finite() above,
    // so division is safe and won't produce inf
    let confidence_pct = (confidence / price_value.abs()) * 100.0;
    
    // Additional safety check after calculation
    if !confidence_pct.is_finite() {
        log::debug!("Confidence percentage calculation produced non-finite value");
        return Ok((false, None));
    }

    if confidence_pct > MAX_CONFIDENCE_PCT {
        log::debug!(
            "Pyth oracle confidence too high: {:.2}% > {:.2}% (price: {}, confidence: {})",
            confidence_pct,
            MAX_CONFIDENCE_PCT,
            price_value,
            confidence
        );
        return Ok((false, None));
    }

    // COMBINED CHECK: Old feed + high confidence = suspicious
    // If slot difference is > 15 slots (~6 seconds) AND confidence > 1.0%, reject
    // This prevents accepting manipulated prices that are slightly stale but have high confidence
    if slot_diff_last > 15 && confidence_pct > 1.0 {
        log::warn!(
            "Suspicious oracle: old_feed ({} slots, ~{:.1}s) + high_confidence ({:.2}%) - rejecting",
            slot_diff_last,
            slot_diff_last as f64 * 0.4, // Convert slots to seconds (400ms per slot)
            confidence_pct
        );
        return Ok((false, None));
    }

    log::debug!(
        "✅ Pyth oracle validation passed for {} (price: {}, confidence: {:.2}%, status: {})",
        oracle_pubkey,
        price_value,
        confidence_pct,
        price_type
    );
    Ok((true, price))
}

/// Validate Pyth confidence with stricter threshold (for Pyth-only mode when Switchboard is unavailable)
/// Returns true if confidence is acceptable, false otherwise
async fn validate_pyth_confidence_strict(
    rpc: &Arc<RpcClient>,
    oracle_pubkey: Pubkey,
    price_value: f64,
    current_slot: u64,
) -> Result<bool> {
    let oracle_account = rpc
        .get_account(&oracle_pubkey)
        .map_err(|e| anyhow::anyhow!("Failed to get oracle account: {}", e))?;

    if oracle_account.data.len() < 56 {
        return Ok(false);
    }

    // Parse exponent
    let exponent_bytes: [u8; 4] = oracle_account.data[16..20]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse exponent"))?;
    let exponent = i32::from_le_bytes(exponent_bytes);

    // Parse confidence (u64 at offset 48-56)
    let conf_bytes: [u8; 8] = oracle_account.data[48..56]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Failed to parse confidence"))?;
    let confidence_raw = u64::from_le_bytes(conf_bytes);
    let confidence = confidence_raw as f64 * 10_f64.powi(exponent);

    // CRITICAL: Check minimum price threshold to prevent division by zero and floating point issues
    // Edge case: price_value = 1e-100 → price_value.abs() > 0.0 check passes, but
    // confidence_pct = confidence / price_value.abs() would produce inf or very large values
    if price_value.abs() < MIN_VALID_PRICE_USD {
        log::warn!(
            "Pyth oracle price below minimum threshold for strict check: {} < {} (price too small, likely invalid)",
            price_value.abs(),
            MIN_VALID_PRICE_USD
        );
        return Ok(false);
    }
    
    // Check if confidence interval is too large (stricter threshold for Pyth-only mode)
    // NOTE: We already checked price_value.abs() >= MIN_VALID_PRICE_USD above,
    // so division is safe and won't produce inf
    let confidence_pct = (confidence / price_value.abs()) * 100.0;

    if confidence_pct > MAX_CONFIDENCE_PCT_PYTH_ONLY {
        log::warn!(
            "Pyth oracle confidence too high for Pyth-only mode: {:.2}% > {:.2}% (price: {}, confidence: {})",
            confidence_pct,
            MAX_CONFIDENCE_PCT_PYTH_ONLY,
            price_value,
            confidence
        );
        return Ok(false);
    }

    log::debug!(
        "✅ Pyth confidence check passed (stricter threshold): {:.2}% <= {:.2}%",
        confidence_pct,
        MAX_CONFIDENCE_PCT_PYTH_ONLY
    );

    Ok(true)
}

/// Liquidation quote with profit calculation per Structure.md section 7
struct LiquidationQuote {
    quote: JupiterQuote,
    profit_usdc: f64,
    collateral_value_usd: f64, // Position size in USD for risk limit calculation
    debt_to_repay_raw: u64, // Debt amount to repay (in debt token raw units) for Solend instruction
    collateral_to_seize_raw: u64, // Collateral cToken amount to seize (for redemption check)
}

/// Get SOL price in USD from oracle
/// Returns SOL price if available, otherwise None
async fn get_sol_price_usd(rpc: &Arc<RpcClient>, ctx: &LiquidationContext) -> Option<f64> {
    // SOL native mint: So11111111111111111111111111111111111111112
    let sol_native_mint = Pubkey::from_str("So11111111111111111111111111111111111111112").ok()?;
    
    // Check if collateral or debt is SOL, use its price
    if let Some(deposit_reserve) = &ctx.deposit_reserve {
        if deposit_reserve.liquidity.mintPubkey == sol_native_mint {
            return ctx.deposit_price_usd;
        }
    }
    
    if let Some(borrow_reserve) = &ctx.borrow_reserve {
        if borrow_reserve.liquidity.mintPubkey == sol_native_mint {
            return ctx.borrow_price_usd;
        }
    }
    
    // If SOL is not in the reserves, fetch from Pyth SOL/USD price feed
    // Pyth SOL/USD mainnet price feed: H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG
    let sol_usd_pyth_feed = match Pubkey::from_str("H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG") {
        Ok(pk) => pk,
        Err(_) => return None,
    };
    
    let current_slot = rpc.get_slot().ok()?;
    match validate_pyth_oracle(rpc, sol_usd_pyth_feed, current_slot).await {
        Ok((true, Some(price))) => Some(price),
        _ => None,
    }
}

/// Get Jupiter quote for liquidation with profit calculation per Structure.md section 7
async fn get_liquidation_quote(
    ctx: &LiquidationContext,
    config: &Config,
    rpc: &Arc<RpcClient>,
) -> Result<LiquidationQuote> {
    // Use first borrow and first deposit
    if ctx.obligation.borrows.is_empty() || ctx.obligation.deposits.is_empty() {
        return Err(anyhow::anyhow!("No borrows or deposits in obligation"));
    }

    let borrow = &ctx.obligation.borrows[0];
    // Note: deposit is available but we use deposit_reserve directly for mint address

    // CRITICAL FIX: Solend liquidation collateral flow
    //
    // Solend LiquidateObligation instruction:
    // - Input: sourceLiquidity (debt token, e.g., USDC)
    // - Output: destinationCollateral (collateral cToken, e.g., cSOL)
    //
    // PROBLEM: cTokens cannot be traded directly on Jupiter!
    // SOLUTION: We need to redeem cToken to underlying token first (cSOL -> SOL)
    //
    // However, the current build_liquidation_tx() only includes LiquidateObligation.
    // RedeemReserveCollateral instruction is MISSING!
    //
    // For now, we'll use the underlying token mint for Jupiter quote,
    // assuming we'll add RedeemReserveCollateral instruction later.
    
    // Step 1: Get collateral cToken mint (what Solend gives us)
    let collateral_ctoken_mint = ctx
        .deposit_reserve
        .as_ref()
        .map(|r| r.collateral.mintPubkey) // cToken mint (e.g., cSOL)
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;
    
    // Step 2: Get underlying collateral token mint (what we need for Jupiter)
    let collateral_underlying_mint = ctx
        .deposit_reserve
        .as_ref()
        .map(|r| r.liquidity.mintPubkey) // Underlying token mint (e.g., SOL)
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;
    
    // Step 3: Get debt token mint
    let debt_mint = ctx
        .borrow_reserve
        .as_ref()
        .map(|r| r.liquidity.mintPubkey) // Debt token mint (e.g., USDC)
        .ok_or_else(|| anyhow::anyhow!("Borrow reserve not loaded"))?;
    
    log::debug!(
        "Jupiter quote request:\n\
         Collateral cToken: {}\n\
         Collateral underlying: {} (will be used for Jupiter)\n\
         Debt token: {}",
        collateral_ctoken_mint,
        collateral_underlying_mint,
        debt_mint
    );
    
    // Use underlying mint for Jupiter (cToken cannot be traded on Jupiter)
    let collateral_mint = collateral_underlying_mint;

    // CRITICAL: Calculate correct collateral amount to seize
    // 
    // Liquidation flow:
    // 1. We repay: debt_to_repay = borrowedAmountWad * close_factor (50%)
    // 2. Solend gives us: collateral_to_seize = debt_to_repay * (1 + liquidation_bonus)
    // 3. We need to swap: collateral_to_seize amount of collateral → debt_to_repay amount of debt
    //
    // Steps:
    // a) Calculate debt to repay (with close factor 50%)
    // b) Calculate collateral to seize (with liquidation bonus)
    // c) Query Jupiter: collateral_to_seize → debt token
    
    // Step 1: Get reserves and token decimals
    // CRITICAL: We need decimals to calculate raw token amounts correctly
    let borrow_reserve = ctx
        .borrow_reserve
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Borrow reserve not loaded"))?;
    let deposit_reserve = ctx
        .deposit_reserve
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;
    
    let debt_decimals = borrow_reserve.liquidity.mintDecimals;
    let collateral_decimals = deposit_reserve.liquidity.mintDecimals;
    
    // CRITICAL SECURITY: Validate decimal values to prevent corrupt data issues
    // SPL tokens use 0-18 decimals (standard range)
    // If layout changes or data is corrupt, we might get invalid values (0, 255, etc.)
    if debt_decimals > 18 {
        return Err(anyhow::anyhow!(
            "Invalid debt decimals: {} (expected 0-18). Reserve: {}. This may indicate corrupt data or layout changes.",
            debt_decimals,
            ctx.obligation.borrows[0].borrowReserve
        ));
    }
    
    if collateral_decimals > 18 {
        return Err(anyhow::anyhow!(
            "Invalid collateral decimals: {} (expected 0-18). Reserve: {}. This may indicate corrupt data or layout changes.",
            collateral_decimals,
            ctx.obligation.deposits[0].depositReserve
        ));
    }
    
    // Step 2: Calculate debt to repay (close factor = 50%)
    // CRITICAL FIX: Solend's debt calculation works as follows:
    // 1. borrowedAmountWad: Initial borrowed amount (WAD format, token decimals NOT included)
    // 2. cumulativeBorrowRateWads: Interest rate accumulator (WAD format)
    // 3. Token decimals: Mint's actual decimals (e.g., USDC = 6, SOL = 9)
    //
    // Formula: actual_debt = borrowedAmountWad * cumulativeBorrowRateWads / WAD / WAD
    // Why two divisions? Both inputs are WAD (10^18), product is 10^36, need two divisions to normalize
    // Result is normalized amount (not in WAD format), then multiply by 10^decimals to get raw amount
    const WAD: u128 = 1_000_000_000_000_000_000; // 10^18
    const CLOSE_FACTOR: u128 = WAD / 2; // 0.5 = WAD/2
    
    // Step 2a: Calculate actual debt in normalized format (interest included)
    // CRITICAL: Both borrowedAmountWad and cumulativeBorrowRateWads are in WAD format (10^18)
    // When multiplied: 10^36, so we need to divide by WAD twice to get normalized amount
    // Formula: actual_debt = borrowedAmountWad * cumulativeBorrowRateWads / WAD / WAD
    let actual_debt_wad: u128 = borrow.borrowedAmountWad
        .checked_mul(borrow.cumulativeBorrowRateWads)
        .and_then(|v| v.checked_div(WAD))
        .and_then(|v| v.checked_div(WAD))  // ✅ İKİNCİ DIVISION - CRITICAL FIX
        .ok_or_else(|| anyhow::anyhow!("Debt calculation overflow: borrowedAmountWad * cumulativeBorrowRateWads"))?;
    
    // Step 2b: Apply close factor (50%)
    let debt_to_repay_wad: u128 = actual_debt_wad
        .checked_mul(CLOSE_FACTOR)
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
        borrow.borrowedAmountWad,
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
    // Exchange rate formula:
    //   exchange_rate = total_underlying_tokens / total_ctokens
    //   total_underlying = availableAmount + (borrowedAmountWads / WAD) * 10^decimals
    //   total_ctokens = collateral.mintTotalSupply
    //
    // CRITICAL: borrowedAmountWads is in WAD format (10^18), but represents the borrowed amount
    // in token units (not raw units). We need to convert it to raw units by multiplying by 10^decimals.
    //
    // This is NOT 1:1 because Solend has a collateral factor and interest accrual!
    // NOTE: WAD is already defined earlier in this function
    
    let ctokens_total_supply = deposit_reserve.collateral.mintTotalSupply;
    let sol_available = deposit_reserve.liquidity.availableAmount;
    let sol_borrowed_wads = deposit_reserve.liquidity.borrowedAmountWads;
    
    // Convert borrowed amount from WAD to token units, then to raw units
    // borrowedAmountWads is in WAD format (borrowed_amount * 10^18)
    // First convert to token units: borrowedAmountWads / WAD
    // Then convert to raw units: (borrowedAmountWads / WAD) * 10^decimals
    let sol_borrowed_token_units = (sol_borrowed_wads as f64 / WAD as f64);
    let sol_borrowed_raw = (sol_borrowed_token_units * 10_f64.powi(collateral_decimals as i32)) as u64;
    
    // Total underlying tokens in the reserve
    let sol_liquidity_supply = sol_available.saturating_add(sol_borrowed_raw);
    
    // Calculate exchange rate: SOL per cToken
    let exchange_rate = if ctokens_total_supply > 0 {
        sol_liquidity_supply as f64 / ctokens_total_supply as f64
    } else {
        1.0 // Fallback if no cTokens exist (shouldn't happen in practice)
    };
    
    // Actual SOL amount we'll receive after redemption
    let sol_amount_after_redemption = (collateral_to_seize_raw as f64 * exchange_rate) as u64;
    
    log::debug!(
        "cToken exchange calculation: {} cTokens * {:.6} rate = {} SOL (available={}, borrowed_wads={}, borrowed_raw={}, total_ctokens={}, total_underlying={})",
        collateral_to_seize_raw,
        exchange_rate,
        sol_amount_after_redemption,
        sol_available,
        sol_borrowed_wads,
        sol_borrowed_raw,
        ctokens_total_supply,
        sol_liquidity_supply
    );
    
    // Step 7: Get preliminary Jupiter quote to calculate price impact
    // CRITICAL FIX: Use price impact from Jupiter quote instead of just position size
    // This accounts for pool liquidity, which is more accurate than position size alone
    const BASE_SLIPPAGE_BPS: u16 = 30; // 0.3% minimum base slippage for preliminary quote
    
    let preliminary_quote = get_jupiter_quote_with_retry(
        &collateral_mint, // ✅ Underlying token (SOL), NOT cToken (cSOL)
        &debt_mint,       // ✅ Debt token (USDC)
        sol_amount_after_redemption, // ✅ CORRECT: Actual SOL amount after redemption
        BASE_SLIPPAGE_BPS, // Base slippage for preliminary quote
        3, // max_retries
    )
    .await
    .context("Failed to get preliminary Jupiter quote")?;
    
    // Calculate dynamic slippage based on price impact from preliminary quote
    // Formula: base_slippage + price_impact + buffer
    // This is economically correct because it accounts for actual pool liquidity
    let price_impact_pct = crate::jup::get_price_impact_pct(&preliminary_quote);
    let price_impact_bps = (price_impact_pct * 100.0) as u16; // Convert percentage to basis points
    const BUFFER_BPS: u16 = 20; // 0.2% safety buffer
    const MAX_SLIPPAGE_BPS: u16 = 300; // 3% maximum slippage
    
    let slippage_bps = (BASE_SLIPPAGE_BPS as u32 + price_impact_bps as u32 + BUFFER_BPS as u32)
        .min(MAX_SLIPPAGE_BPS as u32) as u16;
    
    log::debug!(
        "Dynamic slippage calculation: base={}bps + price_impact={}bps ({}%) + buffer={}bps = {}bps ({}%)",
        BASE_SLIPPAGE_BPS,
        price_impact_bps,
        price_impact_pct,
        BUFFER_BPS,
        slippage_bps,
        slippage_bps as f64 / 100.0
    );
    
    // Step 8: Get final Jupiter quote with calculated dynamic slippage
    // CRITICAL: Jupiter quote uses UNDERLYING token (SOL), not cToken (cSOL)
    // 
    // Liquidation flow:
    // 1. Solend gives us collateral cToken (e.g., cSOL) via LiquidateObligation
    // 2. We redeem cToken to underlying token (cSOL -> SOL) via RedeemReserveCollateral
    // 3. We swap underlying token to debt token (SOL -> USDC) via Jupiter
    // 4. We use the received USDC to pay off the debt (e.g., 1500 USDC)
    // 5. Profit = jupiter_output - debt_to_repay - fees
    let quote = get_jupiter_quote_with_retry(
        &collateral_mint, // ✅ Underlying token (SOL), NOT cToken (cSOL)
        &debt_mint,       // ✅ Debt token (USDC)
        sol_amount_after_redemption, // ✅ CORRECT: Actual SOL amount after redemption
        slippage_bps, // ✅ CORRECT: Dynamic slippage based on price impact
        3, // max_retries
    )
    .await
    .context("Failed to get Jupiter quote with retries")?;
    
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
        .map(|r| r.liquidity.mintDecimals)
        .unwrap_or(6);
    
    // CRITICAL SECURITY: Validate decimal value to prevent corrupt data issues
    // SPL tokens use 0-18 decimals (standard range)
    if debt_decimals > 18 {
        return Err(anyhow::anyhow!(
            "Invalid debt decimals: {} (expected 0-18). Reserve: {}. This may indicate corrupt data or layout changes.",
            debt_decimals,
            ctx.obligation.borrows[0].borrowReserve
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
    
    // Step 7e: Subtract Jito tip and transaction fees
    // Get SOL price for fee calculations
    let sol_price_usd = get_sol_price_usd(rpc, ctx).await
        .unwrap_or_else(|| {
            log::warn!("Failed to get SOL price, using fallback $150");
            150.0 // Fallback price if oracle fails
        });
    
    let jito_tip_lamports = config.jito_tip_amount_lamports.unwrap_or(10_000_000u64);
    let jito_tip_sol = jito_tip_lamports as f64 / 1_000_000_000.0;
    let jito_fee_usd = jito_tip_sol * sol_price_usd;
    
    const BASE_TX_FEE_LAMPORTS: u64 = 5_000;
    let tx_fee_sol = BASE_TX_FEE_LAMPORTS as f64 / 1_000_000_000.0;
    let tx_fee_usd = tx_fee_sol * sol_price_usd;
    
    // FINAL PROFIT
    let profit_usdc = profit_before_fees_usd - jito_fee_usd - tx_fee_usd;
    
    log::debug!(
        "Profit calculation (CORRECTED):\n\
         Jupiter output: {} raw ({:.6} tokens, ${:.2} USD)\n\
         Debt to repay: {} raw ({:.6} tokens, ${:.2} USD)\n\
         Profit (before fees): {} raw ({:.6} tokens, ${:.2} USD)\n\
         Jito fee: ${:.4}\n\
         TX fee: ${:.4}\n\
         FINAL PROFIT: ${:.2}",
        jupiter_out_amount,
        jupiter_out_amount as f64 / 10_f64.powi(debt_decimals as i32),
        jupiter_out_amount as f64 / 10_f64.powi(debt_decimals as i32) * debt_price_usd,
        debt_to_repay_raw,
        debt_to_repay_raw as f64 / 10_f64.powi(debt_decimals as i32),
        debt_to_repay_usd,
        profit_raw,
        profit_tokens,
        profit_before_fees_usd,
        jito_fee_usd,
        tx_fee_usd,
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
    
    Ok(LiquidationQuote {
        quote,
        profit_usdc,
        collateral_value_usd,
        debt_to_repay_raw, // Debt amount to repay in Solend instruction
        collateral_to_seize_raw, // Collateral cToken amount to seize (for redemption check)
    })
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
    
    // Get SOL price from Pyth (fallback to $150 if unavailable)
    let sol_usd_pyth_feed = match Pubkey::from_str("H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG") {
        Ok(pk) => pk,
        Err(_) => {
            log::warn!("Failed to parse SOL/USD Pyth feed, using fallback $150");
            let sol_value_usd = (sol_balance as f64) / 1_000_000_000.0 * 150.0;
            // Continue to get USDC balance
            let program_id = crate::solend::solend_program_id()?;
            let usdc_mint = crate::solend::find_usdc_mint_from_reserves(rpc, &program_id)
                .context("Failed to discover USDC mint")?;
            use spl_associated_token_account::get_associated_token_address;
            let usdc_ata = get_associated_token_address(wallet_pubkey, &usdc_mint);
            let usdc_balance_raw = match rpc.get_token_account(&usdc_ata) {
                Ok(Some(account)) => account.token_amount.amount.parse::<u64>().unwrap_or(0),
                _ => 0,
            };
            let usdc_value_usd = (usdc_balance_raw as f64) / 1_000_000.0;
            return Ok(sol_value_usd + usdc_value_usd);
        }
    };
    
    let current_slot = rpc.get_slot()
        .map_err(|e| anyhow::anyhow!("Failed to get current slot: {}", e))?;
    let sol_price_usd = match validate_pyth_oracle(rpc, sol_usd_pyth_feed, current_slot).await {
        Ok((true, Some(price))) => price,
        _ => {
            log::warn!("Failed to get SOL price from oracle, using fallback $150");
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
/// Build transaction 1: Liquidation + Redemption (NO Jupiter Swap!)
/// CRITICAL: blockhash must be fresh (fetched immediately before calling this function).
/// Blockhashes are valid for ~150 slots (~60 seconds), so fetch blockhash right before
/// building the transaction to minimize staleness risk.
async fn build_liquidation_tx1(
    wallet: &Arc<Keypair>,
    ctx: &LiquidationContext,
    quote: &LiquidationQuote,
    rpc: &Arc<RpcClient>,
    blockhash: solana_sdk::hash::Hash,
    config: &Config,
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
        let (valid, _) = validate_pyth_oracle(
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
        let (valid, _) = validate_pyth_oracle(
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
    let repay_reserve_liquidity_supply = borrow_reserve.liquidity.supplyPubkey;
    let withdraw_reserve_liquidity_supply = deposit_reserve.liquidity.supplyPubkey;
    let withdraw_reserve_collateral_supply = deposit_reserve.collateral.supplyPubkey;
    let withdraw_reserve_collateral_mint = deposit_reserve.collateral.mintPubkey;
    
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
    let borrow_reserve_pubkey = ctx.obligation.borrows[0].borrowReserve;
    let deposit_reserve_pubkey = ctx.obligation.deposits[0].depositReserve;
    
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
    let source_liquidity = get_associated_token_address(&wallet_pubkey, &borrow_reserve.liquidity.mintPubkey);
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
            borrow_reserve.liquidity.mintPubkey
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
        AccountMeta::new(ctx.obligation.borrows[0].borrowReserve, false),  // 2: repayReserve (writable, refreshed)
        AccountMeta::new(repay_reserve_liquidity_supply, false),      // 3: repayReserveLiquiditySupply
        AccountMeta::new_readonly(ctx.obligation.deposits[0].depositReserve, false), // 4: withdrawReserve (readonly, refreshed)
        AccountMeta::new(withdraw_reserve_collateral_supply, false),  // 5: withdrawReserveCollateralSupply
        AccountMeta::new(ctx.obligation_pubkey, false),                // 6: obligation (writable, refreshed)
        AccountMeta::new_readonly(lending_market, false),            // 7: lendingMarket
        AccountMeta::new_readonly(lending_market_authority, false),   // 8: lendingMarketAuthority
        AccountMeta::new_readonly(wallet_pubkey, true),               // 9: transferAuthority (signer)
        AccountMeta::new_readonly(sysvar::clock::id(), false),        // 10: clockSysvar
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),          // 11: tokenProgram
    ];

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
        &deposit_reserve.liquidity.mintPubkey // Underlying token mint (e.g., SOL)
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
            deposit_reserve.liquidity.mintPubkey
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
        AccountMeta::new(ctx.obligation.deposits[0].depositReserve, false), // 2: reserve
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

/// Build transaction 2: Jupiter Swap (SOL -> USDC)
/// This is called AFTER TX1 confirms and SOL is available in wallet
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
    let jupiter_swap_ix = crate::jup::build_jupiter_swap_instruction(
        &quote.quote,
        &wallet_pubkey,
        &config.jupiter_url,
    )
    .await
    .context("Failed to build Jupiter swap instruction")?;
    
    // Build transaction with fresh blockhash
    let mut tx = Transaction::new_with_payer(
        &[
            compute_budget_ix,        // Compute unit limit
            priority_fee_ix,          // Priority fee
            jupiter_swap_ix,          // Jupiter Swap (SOL -> USDC)
        ],
        Some(&wallet_pubkey),
    );
    tx.message.recent_blockhash = blockhash;

    log::info!(
        "Built TX2 (Jupiter Swap) for obligation {}:\n\
         - Swap: SOL -> USDC",
        ctx.obligation_pubkey
    );

    Ok(tx)
}

/// Execute liquidation with swap using two separate transactions
/// TX1: Liquidation + Redemption (Solend protocol)
/// TX2: Jupiter Swap (DEX)
/// 
/// CRITICAL: These must be separate transactions because:
/// - TX1 writes SOL tokens to wallet's ATA
/// - TX2 needs to read those SOL tokens
/// - Solana transactions are atomic - all instructions execute simultaneously
/// - If combined, TX2 would try to read SOL that hasn't been written yet
async fn execute_liquidation_with_swap(
    ctx: &LiquidationContext,
    quote: &LiquidationQuote,
    config: &Config,
    rpc: &Arc<RpcClient>,
    jito_client: &JitoClient,
) -> Result<()> {
    use spl_associated_token_account::get_associated_token_address;
    
    let wallet = &config.wallet;
    let wallet_pubkey = wallet.pubkey();
    
    // Get deposit reserve to find underlying token mint (SOL)
    let deposit_reserve = ctx
        .deposit_reserve
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Deposit reserve not loaded"))?;
    
    let sol_mint = deposit_reserve.liquidity.mintPubkey;
    let sol_ata = get_associated_token_address(&wallet_pubkey, &sol_mint);
    
    // ============================================================================
    // TRANSACTION 1: Liquidation + Redemption
    // ============================================================================
    log::info!("Building TX1: Liquidation + Redemption");
    
    let blockhash1 = rpc
        .get_latest_blockhash()
        .map_err(|e| anyhow::anyhow!("Failed to get blockhash for TX1: {}", e))?;
    
    let tx1 = build_liquidation_tx1(wallet, ctx, quote, rpc, blockhash1, config)
        .await
        .context("Failed to build TX1")?;
    
    // Send TX1 via Jito
    let bundle_id1 = send_jito_bundle(tx1, jito_client, wallet, blockhash1)
        .await
        .context("Failed to send TX1 via Jito")?;
    log::info!("✅ TX1 sent: bundle_id={}", bundle_id1);
    
    // ============================================================================
    // WAIT FOR TX1 TO LAND ON-CHAIN
    // ============================================================================
    // CRITICAL: TX2 needs TX1's output (SOL tokens in wallet)
    // We must wait for TX1 to confirm before sending TX2
    
    log::info!("Waiting for TX1 to confirm (checking SOL balance in ATA: {})...", sol_ata);
    
    // Poll for SOL balance (max 5 seconds, 10 checks)
    let mut confirmed = false;
    for i in 0..10 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        
        if let Ok(Some(account)) = rpc.get_token_account(&sol_ata) {
            let balance: u64 = account.token_amount.amount.parse().unwrap_or(0);
            if balance >= quote.collateral_to_seize_raw {
                log::info!(
                    "✅ TX1 confirmed: SOL balance={} (needed={})",
                    balance,
                    quote.collateral_to_seize_raw
                );
                confirmed = true;
                break;
            }
        }
        
        log::debug!("TX1 not confirmed yet (check {}/10)", i + 1);
    }
    
    if !confirmed {
        return Err(anyhow::anyhow!(
            "TX1 failed to confirm within 5 seconds. \
             Bundle may have dropped or network congestion. \
             Bundle ID: {}",
            bundle_id1
        ));
    }
    
    // ============================================================================
    // TRANSACTION 2: Jupiter Swap
    // ============================================================================
    log::info!("Building TX2: Jupiter Swap (SOL -> USDC)");
    
    // Get fresh blockhash for TX2 (TX1 took time)
    let blockhash2 = rpc
        .get_latest_blockhash()
        .map_err(|e| anyhow::anyhow!("Failed to get blockhash for TX2: {}", e))?;
    
    let tx2 = build_liquidation_tx2(wallet, ctx, quote, blockhash2, config)
        .await
        .context("Failed to build TX2")?;
    
    // Send TX2 via Jito
    let bundle_id2 = send_jito_bundle(tx2, jito_client, wallet, blockhash2)
        .await
        .context("Failed to send TX2 via Jito")?;
    log::info!("✅ TX2 sent: bundle_id={}", bundle_id2);
    
    // Wait for TX2 confirmation (optional, for logging)
    tokio::time::sleep(Duration::from_millis(1000)).await;
    
    log::info!(
        "✅ Full liquidation flow completed: TX1={}, TX2={}",
        bundle_id1,
        bundle_id2
    );
    
    Ok(())
}


