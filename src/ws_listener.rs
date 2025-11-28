use anyhow::{Context, Result};
use crate::config::Config;
use crate::event::Event;
use crate::event_bus::EventBus;
use crate::health::HealthManager;
use crate::protocol::Protocol;
use crate::solana_client::SolanaClient;
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;
use solana_sdk::pubkey::Pubkey;
use std::sync::Arc;
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
    jsonrpc: String,
    id: Option<u64>,
    result: Option<u64>, // Subscription ID
    error: Option<SubscribeError>,
}

#[derive(Deserialize, Debug)]
struct SubscribeError {
    code: i32,
    message: String,
}

/// Account update notification from Solana PubSub API
#[derive(Deserialize, Debug)]
struct AccountNotification {
    jsonrpc: String,
    method: String,
    params: AccountNotificationParams,
}

#[derive(Deserialize, Debug)]
struct AccountNotificationParams {
    result: AccountNotificationResult,
    subscription: u64,
}

#[derive(Deserialize, Debug)]
struct AccountNotificationResult {
    #[serde(rename = "account")]
    account: AccountData,
    #[serde(rename = "context")]
    context: NotificationContext,
}

#[derive(Deserialize, Debug)]
struct NotificationContext {
    slot: u64,
}

#[derive(Deserialize, Debug)]
struct AccountData {
    data: Vec<String>, // [base64_data, encoding]
    executable: bool,
    lamports: u64,
    owner: String,
    rent_epoch: u64,
}

/// WebSocket listener - account değişikliklerini dinler
/// 
/// Solana WebSocket PubSub API kullanarak program account'larını real-time dinler.
/// Bu yaklaşım RPC polling'den çok daha hızlıdır (<100ms latency) ve rate limit sorunu yoktur.
pub async fn run_ws_listener(
    bus: EventBus,
    config: Config,
    rpc_client: Arc<SolanaClient>,
    protocol: Arc<dyn Protocol>,
    health_manager: Arc<HealthManager>,
) -> Result<()> {
    let program_id = protocol.program_id();
    let ws_url = config.rpc_ws_url.clone();
    let mut subscription_id_counter = 1u64;
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

    loop {
        match connect_and_listen(
            &ws_url,
            &program_id,
            &bus,
            Arc::clone(&rpc_client),
            protocol.as_ref(),
            Arc::clone(&health_manager),
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

async fn connect_and_listen(
    ws_url: &str,
    program_id: &Pubkey,
    bus: &EventBus,
    rpc_client: Arc<SolanaClient>,
    protocol: &dyn Protocol,
    health_manager: Arc<HealthManager>,
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
                                log::info!("✅ Subscribed to program accounts (subscription ID: {})", sub_id);
                                consecutive_errors = 0; // Reset on successful subscription
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

                        // Try to parse as account notification
                        // Note: Solana programSubscribe notifications have a different structure
                        // We need to extract the account pubkey from the notification
                        if let Ok(notification) = serde_json::from_str::<AccountNotification>(&text) {
                            if notification.method == "accountNotification" || notification.method == "programNotification" {
                                if let Some(sub_id) = subscription_id {
                                    if notification.params.subscription == sub_id {
                                        // For programSubscribe, we need to get the account pubkey
                                        // The account pubkey is typically in the notification params
                                        // Let's parse it from the raw JSON
                                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                                            if let Some(params) = parsed.get("params") {
                                                if let Some(result) = params.get("result") {
                                                    // Try to find account pubkey in the result
                                                    // For programSubscribe, the account pubkey might be in a different field
                                                    // Let's use a more flexible approach
                                                    handle_program_notification(
                                                        &text,
                                                        &notification.params.result.account,
                                                        bus,
                                                        Arc::clone(&rpc_client),
                                                        protocol,
                                                        Arc::clone(&health_manager),
                                                    )
                                                    .await?;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            continue;
                        }
                        
                        // Also try to parse as a generic notification (fallback)
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                            if let Some(method) = parsed.get("method").and_then(|m| m.as_str()) {
                                if method == "programNotification" || method == "accountNotification" {
                                    if let Some(params) = parsed.get("params") {
                                        if let Some(result) = params.get("result") {
                                            // Extract account data
                                            if let Some(account_data) = result.get("account") {
                                                if let Ok(account) = serde_json::from_value::<AccountData>(account_data.clone()) {
                                                    // Try to extract account pubkey from the notification
                                                    // For programSubscribe, we might need to get it from a different field
                                                    // For now, we'll need to use a workaround
                                                    handle_program_notification(
                                                        &text,
                                                        &account,
                                                        bus,
                                                        Arc::clone(&rpc_client),
                                                        protocol,
                                                        Arc::clone(&health_manager),
                                                    )
                                                    .await?;
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
    raw_json: &str,
    account_data: &AccountData,
    bus: &EventBus,
    rpc_client: Arc<SolanaClient>,
    protocol: &dyn Protocol,
    health_manager: Arc<HealthManager>,
) -> Result<()> {
    // For programSubscribe, we need to extract the account pubkey from the notification
    // The account pubkey is typically in params.result.account or a similar field
    // Let's parse the raw JSON to find it
    let parsed: serde_json::Value = serde_json::from_str(raw_json)
        .context("Failed to parse notification JSON")?;
    
    // Try to find account pubkey in various possible locations
    let account_pubkey = if let Some(params) = parsed.get("params") {
        if let Some(result) = params.get("result") {
            // Check if there's a pubkey field
            if let Some(pubkey_str) = result.get("pubkey").and_then(|p| p.as_str()) {
                pubkey_str.parse::<Pubkey>().ok()
            } else if let Some(account_obj) = result.get("account") {
                // Sometimes the pubkey is nested in the account object
                account_obj.get("pubkey").and_then(|p| p.as_str())
                    .and_then(|s| s.parse::<Pubkey>().ok())
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    // If we couldn't find the pubkey in the notification, we can't process it
    // This is a limitation - we need the account address to parse the position
    // In a production system, you might want to fetch all program accounts initially
    // and maintain a mapping, or use a different subscription method
    let account_pubkey = match account_pubkey {
        Some(pk) => pk,
        None => {
            log::debug!("Could not extract account pubkey from notification, skipping");
            return Ok(());
        }
    };

    // Decode base64 account data
    let account_bytes = base64::decode(&account_data.data[0])
        .context("Failed to decode base64 account data")?;

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
