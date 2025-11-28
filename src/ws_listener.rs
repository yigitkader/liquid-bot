use anyhow::{Context, Result};
use crate::config::Config;
use crate::event::Event;
use crate::event_bus::EventBus;
use crate::health::HealthManager;
use crate::protocol::Protocol;
use crate::solana_client::SolanaClient;
use base64::{Engine as _, engine::general_purpose};
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use sha2::Digest;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// WebSocket subscription request for Solana PubSub API
#[derive(Serialize)]
struct SubscribeRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    params: Vec<serde_json::Value>,
}

/// WebSocket subscription response from Solana PubSub API
#[derive(Deserialize, Debug)]
struct SubscribeResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    #[allow(dead_code)]
    id: Option<u64>,
    result: Option<u64>, // Subscription ID
    error: Option<SubscribeError>,
}

#[derive(Deserialize, Debug)]
struct SubscribeError {
    code: i32,
    message: String,
}

/// Program notification from Solana PubSub API
/// Format: https://docs.solana.com/api/websocket#programsubscribe
#[derive(Deserialize, Debug)]
struct ProgramNotification {
    #[allow(dead_code)]
    jsonrpc: String,
    method: String,
    params: ProgramNotificationParams,
}

#[derive(Deserialize, Debug)]
struct ProgramNotificationParams {
    result: ProgramNotificationResult,
    subscription: u64,
}

#[derive(Deserialize, Debug)]
struct ProgramNotificationResult {
    #[serde(rename = "context")]
    #[allow(dead_code)]
    context: NotificationContext,
    #[serde(rename = "value")]
    value: ProgramAccountValue,
}

#[derive(Deserialize, Debug)]
struct NotificationContext {
    #[allow(dead_code)]
    slot: u64,
}

/// Program account value in notification
/// This contains the account pubkey and account data
#[derive(Deserialize, Debug)]
struct ProgramAccountValue {
    #[serde(rename = "pubkey")]
    pubkey: String, // Account pubkey as string
    #[serde(rename = "account")]
    account: AccountData,
}

#[derive(Deserialize, Debug)]
struct AccountData {
    data: Vec<String>, // [base64_data, encoding]
    executable: bool,
    lamports: u64,
    owner: String,
    rent_epoch: u64,
}

/// Account cache for tracking known accounts
/// Used to identify accounts from WebSocket notifications
type AccountCache = Arc<tokio::sync::RwLock<HashMap<String, Pubkey>>>;

/// WebSocket listener - account değişikliklerini dinler
/// 
/// Solana WebSocket PubSub API kullanarak program account'larını real-time dinler.
/// Bu yaklaşım RPC polling'den çok daha hızlıdır (<100ms latency) ve rate limit sorunu yoktur.
/// 
/// Strategy:
/// 1. Initial account discovery: RPC ile tüm program account'larını fetch et ve cache'le
/// 2. WebSocket subscription: programSubscribe ile değişiklikleri dinle
/// 3. Real-time updates: Gelen bildirimleri parse et ve event bus'a yayınla
pub async fn run_ws_listener(
    bus: EventBus,
    config: Config,
    rpc_client: Arc<SolanaClient>,
    protocol: Arc<dyn Protocol>,
    health_manager: Arc<HealthManager>,
) -> Result<()> {
    let program_id = protocol.program_id();
    let ws_url = config.rpc_ws_url.clone();
    let mut reconnect_delay = Duration::from_secs(1);
    let max_reconnect_delay = Duration::from_secs(60);
    let mut consecutive_errors = 0u32;
    let max_consecutive_errors = config.max_consecutive_errors;

    log::info!(
        "📡 Starting WebSocket listener for protocol: {} (program: {})",
        protocol.id(),
        program_id
    );
    log::info!("   WebSocket URL: {}", ws_url);

    // Step 1: Initial account discovery via RPC
    // This ensures we have all existing accounts before starting WebSocket
    log::info!("🔍 Performing initial account discovery via RPC...");
    let account_cache = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
    
    match initial_account_discovery(
        &rpc_client,
        &protocol,
        &bus,
        Arc::clone(&account_cache),
        Arc::clone(&health_manager),
    )
    .await
    {
        Ok(count) => {
            log::info!("✅ Initial discovery complete: {} accounts found and cached", count);
            consecutive_errors = 0; // Reset on successful discovery
        }
        Err(e) => {
            log::warn!("⚠️  Initial discovery failed: {}. Continuing with WebSocket only.", e);
            // Continue anyway - WebSocket will still work for new accounts
        }
    }

    // Step 2: Start WebSocket listener
    let mut subscription_id_counter = 1u64;

    loop {
        match connect_and_listen(
            &ws_url,
            &program_id,
            &bus,
            Arc::clone(&rpc_client),
            protocol.as_ref(),
            Arc::clone(&health_manager),
            Arc::clone(&account_cache),
            &mut subscription_id_counter,
        )
        .await
        {
            Ok(()) => {
                // Normal shutdown (shouldn't happen in production)
                log::info!("WebSocket connection closed normally");
                break;
            }
            Err(e) => {
                consecutive_errors += 1;
                let error_msg = format!("WebSocket error: {}", e);
                log::error!("{} (consecutive errors: {})", error_msg, consecutive_errors);
                health_manager.record_error(error_msg.clone()).await;

                if consecutive_errors >= max_consecutive_errors {
                    log::error!(
                        "Too many consecutive WebSocket errors ({}), stopping listener",
                        consecutive_errors
                    );
                    return Err(anyhow::anyhow!("Too many consecutive WebSocket errors"));
                }

                // Exponential backoff with jitter
                let jitter_ms = rand::thread_rng().gen_range(0..1000);
                let backoff = reconnect_delay + Duration::from_millis(jitter_ms);
                log::warn!("Reconnecting in {:.2}s...", backoff.as_secs_f64());
                sleep(backoff).await;

                // Increase reconnect delay (exponential backoff, capped at max)
                reconnect_delay = (reconnect_delay * 2).min(max_reconnect_delay);
            }
        }
    }

    Ok(())
}

/// Initial account discovery: Fetch all program accounts via RPC and cache them
async fn initial_account_discovery(
    rpc_client: &Arc<SolanaClient>,
    protocol: &Arc<dyn Protocol>,
    bus: &EventBus,
    account_cache: AccountCache,
    health_manager: Arc<HealthManager>,
) -> Result<usize> {
    let program_id = protocol.program_id();

    // Fetch all program accounts
    let accounts = rpc_client
        .get_program_accounts(&program_id)
        .await
        .context("Failed to fetch program accounts for initial discovery")?;

    log::debug!(
        "Fetched {} accounts for initial discovery (program: {})",
        accounts.len(),
        program_id
    );

    let mut position_count = 0;
    let mut cache = account_cache.write().await;

    // Process each account
    for (account_pubkey, account) in accounts {
        // Cache the account pubkey (using account data hash as key for lookup)
        // Note: We hash the raw account data for cache lookup
        let account_data_hash = format!("{:x}", sha2::Sha256::digest(&account.data));
        cache.insert(account_data_hash, account_pubkey);

        // Parse and publish position
        match protocol
            .parse_account_position(&account_pubkey, &account, Some(Arc::clone(rpc_client)))
            .await
        {
            Ok(Some(position)) => {
                bus.publish(Event::AccountUpdated(position))
                    .map_err(|e| anyhow::anyhow!("Failed to publish event: {}", e))?;
                position_count += 1;
            }
            Ok(None) => {
                // This account is not a position (e.g., market account, reserve account, etc.)
                // Ignore it
            }
            Err(e) => {
                log::warn!("Failed to parse account {}: {}", account_pubkey, e);
                // Continue on parse error
            }
        }
    }

    health_manager.record_successful_poll().await;
    Ok(position_count)
}

async fn connect_and_listen(
    ws_url: &str,
    program_id: &Pubkey,
    bus: &EventBus,
    rpc_client: Arc<SolanaClient>,
    protocol: &dyn Protocol,
    health_manager: Arc<HealthManager>,
    account_cache: AccountCache,
    subscription_id_counter: &mut u64,
) -> Result<()> {
    // Connect to WebSocket
    let (ws_stream, _) = connect_async(ws_url)
        .await
        .context("Failed to connect to WebSocket endpoint")?;

    log::info!("✅ WebSocket connected");

    let (mut write, mut read) = ws_stream.split();

    // Subscribe to program accounts
    // Solana PubSub API: programSubscribe method
    // Documentation: https://docs.solana.com/api/websocket#programsubscribe
    let subscribe_request = SubscribeRequest {
        jsonrpc: "2.0".to_string(),
        id: *subscription_id_counter,
        method: "programSubscribe".to_string(),
        params: vec![
            json!(program_id.to_string()),
            json!({
                "encoding": "base64",
                "commitment": "confirmed"
            }),
        ],
    };

    let request_json = serde_json::to_string(&subscribe_request)
        .context("Failed to serialize subscribe request")?;

    log::debug!("Sending subscription request: {}", request_json);

    write
        .send(Message::Text(request_json))
        .await
        .context("Failed to send subscription request")?;

    *subscription_id_counter += 1;

    // Wait for subscription confirmation
    let mut subscription_id: Option<u64> = None;
    let mut confirmed = false;
    let mut consecutive_errors = 0u32;
    const MAX_CONSECUTIVE_NOTIFICATION_ERRORS: u32 = 100; // Max errors before reconnecting

    // Read messages with timeout
    loop {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        log::debug!("Received WebSocket message: {}", text);

                        // Try to parse as subscription response
                        if let Ok(response) = serde_json::from_str::<SubscribeResponse>(&text) {
                            if let Some(sub_id) = response.result {
                                subscription_id = Some(sub_id);
                                confirmed = true;
                                consecutive_errors = 0; // Reset on successful subscription
                                log::info!("✅ Subscribed to program accounts (subscription ID: {})", sub_id);
                                health_manager.record_successful_poll().await;
                            } else if let Some(error) = response.error {
                                return Err(anyhow::anyhow!(
                                    "Subscription error: {} (code: {})",
                                    error.message,
                                    error.code
                                ));
                            }
                            continue;
                        }

                        // Try to parse as program notification
                        // Format: https://docs.solana.com/api/websocket#programsubscribe
                        // The notification contains the account pubkey in params.result.value.pubkey
                        if let Ok(notification) = serde_json::from_str::<ProgramNotification>(&text) {
                            if notification.method == "programNotification" {
                                if let Some(sub_id) = subscription_id {
                                    if notification.params.subscription == sub_id {
                                        // Extract account pubkey from notification
                                        let account_pubkey_str = &notification.params.result.value.pubkey;
                                        match account_pubkey_str.parse::<Pubkey>() {
                                            Ok(account_pubkey) => {
                                                // Process the notification
                                                if let Err(e) = handle_program_notification(
                                                    account_pubkey,
                                                    &notification.params.result.value.account,
                                                    bus,
                                                    Arc::clone(&rpc_client),
                                                    protocol,
                                                    Arc::clone(&health_manager),
                                                    Arc::clone(&account_cache),
                                                )
                                                .await
                                                {
                                                    log::warn!("Failed to handle program notification: {}", e);
                                                    consecutive_errors += 1;
                                                    if consecutive_errors >= MAX_CONSECUTIVE_NOTIFICATION_ERRORS {
                                                        log::error!(
                                                            "Too many consecutive notification errors ({}), reconnecting...",
                                                            consecutive_errors
                                                        );
                                                        return Ok(()); // Will trigger reconnect
                                                    }
                                                } else {
                                                    consecutive_errors = 0; // Reset on success
                                                }
                                            }
                                            Err(e) => {
                                                log::warn!("Failed to parse account pubkey '{}': {}", account_pubkey_str, e);
                                                consecutive_errors += 1;
                                            }
                                        }
                                    }
                                }
                            }
                            continue;
                        }

                        // Fallback: Try to parse as generic JSON to extract pubkey
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                            if let Some(method) = parsed.get("method").and_then(|m| m.as_str()) {
                                if method == "programNotification" {
                                    if let Some(params) = parsed.get("params") {
                                        if let Some(result) = params.get("result") {
                                            if let Some(value) = result.get("value") {
                                                if let Some(pubkey_str) = value.get("pubkey").and_then(|p| p.as_str()) {
                                                    if let Ok(account_pubkey) = pubkey_str.parse::<Pubkey>() {
                                                        if let Some(account_data) = value.get("account") {
                                                            if let Ok(account) = serde_json::from_value::<AccountData>(account_data.clone()) {
                                                                if let Err(e) = handle_program_notification(
                                                                    account_pubkey,
                                                                    &account,
                                                                    bus,
                                                                    Arc::clone(&rpc_client),
                                                                    protocol,
                                                                    Arc::clone(&health_manager),
                                                                    Arc::clone(&account_cache),
                                                                )
                                                                .await
                                                                {
                                                                    log::warn!("Failed to handle program notification (fallback): {}", e);
                                                                    consecutive_errors += 1;
                                                                    if consecutive_errors >= MAX_CONSECUTIVE_NOTIFICATION_ERRORS {
                                                                        log::error!(
                                                                            "Too many consecutive notification errors ({}), reconnecting...",
                                                                            consecutive_errors
                                                                        );
                                                                        return Ok(()); // Will trigger reconnect
                                                                    }
                                                                } else {
                                                                    consecutive_errors = 0;
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // Unknown message format
                        log::debug!("Unknown message format: {}", text);
                    }
                    Some(Ok(Message::Close(_))) => {
                        log::warn!("WebSocket connection closed by server");
                        return Ok(()); // Normal close, will trigger reconnect
                    }
                    Some(Err(e)) => {
                        return Err(anyhow::anyhow!("WebSocket error: {}", e));
                    }
                    None => {
                        return Err(anyhow::anyhow!("WebSocket stream ended"));
                    }
                    _ => {
                        // Binary or other message types - ignore
                    }
                }
            }
            _ = sleep(Duration::from_secs(30)) => {
                // Send ping to keep connection alive
                if confirmed {
                    let ping = json!({
                        "jsonrpc": "2.0",
                        "id": *subscription_id_counter,
                        "method": "ping"
                    });
                    *subscription_id_counter += 1;
                    if let Err(e) = write.send(Message::Text(ping.to_string())).await {
                        log::warn!("Failed to send ping: {}", e);
                        return Err(anyhow::anyhow!("Failed to send ping"));
                    }
                }
            }
        }
    }
}

async fn handle_program_notification(
    account_pubkey: Pubkey,
    account_data: &AccountData,
    bus: &EventBus,
    rpc_client: Arc<SolanaClient>,
    protocol: &dyn Protocol,
    health_manager: Arc<HealthManager>,
    account_cache: AccountCache,
) -> Result<()> {
    // Update account cache
    // Decode account data first, then hash it for cache lookup
    let account_bytes = general_purpose::STANDARD
        .decode(&account_data.data[0])
        .context("Failed to decode account data for cache")?;
    let account_data_hash = format!("{:x}", sha2::Sha256::digest(&account_bytes));
    {
        let mut cache = account_cache.write().await;
        cache.insert(account_data_hash, account_pubkey);
    }

    // Account bytes already decoded above for cache
    // Reuse the decoded bytes

    // Parse owner pubkey
    let owner_pubkey = account_data.owner.parse::<Pubkey>()
        .context("Failed to parse owner pubkey")?;

    // Create Solana account struct
    let account = solana_sdk::account::Account {
        lamports: account_data.lamports,
        data: account_bytes,
        owner: owner_pubkey,
        executable: account_data.executable,
        rent_epoch: account_data.rent_epoch,
    };

    // Parse account position using protocol
    match protocol
        .parse_account_position(&account_pubkey, &account, Some(Arc::clone(&rpc_client)))
        .await
    {
        Ok(Some(position)) => {
            bus.publish(Event::AccountUpdated(position))
                .map_err(|e| anyhow::anyhow!("Failed to publish event: {}", e))?;
            health_manager.record_successful_poll().await;
            log::debug!("Published account update: {}", account_pubkey);
        }
        Ok(None) => {
            // This account is not a position (e.g., market account, reserve account, etc.)
            // Ignore it
        }
        Err(e) => {
            log::warn!("Failed to parse account {}: {}", account_pubkey, e);
            // Continue on parse error
        }
    }

    Ok(())
}
