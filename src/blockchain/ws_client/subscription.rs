// WebSocket subscription management

use serde_json::json;
use solana_sdk::pubkey::Pubkey;
use tokio::sync::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct SubscriptionInfo {
    pub subscription_type: SubscriptionType,
}

#[derive(Debug, Clone)]
pub enum SubscriptionType {
    Program(Pubkey),
    Account(Pubkey),
    Slot,
}

pub struct SubscriptionManager {
    subscriptions: Arc<Mutex<HashMap<u64, SubscriptionInfo>>>,
    failed_subscriptions: Arc<Mutex<Vec<SubscriptionInfo>>>,
}

impl SubscriptionManager {
    pub fn new() -> Self {
        Self {
            subscriptions: Arc::new(Mutex::new(HashMap::new())),
            failed_subscriptions: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn add(&self, id: u64, info: SubscriptionInfo) {
        let mut subs = self.subscriptions.lock().await;
        subs.insert(id, info);
    }

    pub async fn remove(&self, id: &u64) {
        let mut subs = self.subscriptions.lock().await;
        subs.remove(id);
    }

    pub async fn get_all(&self) -> HashMap<u64, SubscriptionInfo> {
        let subs = self.subscriptions.lock().await;
        subs.clone()
    }

    pub async fn clear(&self) {
        let mut subs = self.subscriptions.lock().await;
        subs.clear();
    }

    pub async fn len(&self) -> usize {
        let subs = self.subscriptions.lock().await;
        subs.len()
    }

    pub async fn add_failed(&self, info: SubscriptionInfo) {
        let mut failed = self.failed_subscriptions.lock().await;
        failed.push(info);
    }

    pub async fn get_failed(&self) -> Vec<SubscriptionInfo> {
        let failed = self.failed_subscriptions.lock().await;
        failed.clone()
    }

    pub async fn clear_failed(&self) {
        let mut failed = self.failed_subscriptions.lock().await;
        failed.clear();
    }

    pub fn subscriptions(&self) -> Arc<Mutex<HashMap<u64, SubscriptionInfo>>> {
        Arc::clone(&self.subscriptions)
    }

    pub fn failed_subscriptions(&self) -> Arc<Mutex<Vec<SubscriptionInfo>>> {
        Arc::clone(&self.failed_subscriptions)
    }
}

impl Default for SubscriptionManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Build subscription parameters for different subscription types
/// Solana WebSocket API format:
/// - programSubscribe: [programId, {encoding: "base64", commitment: "confirmed"}]
/// - accountSubscribe: [accountPubkey, {encoding: "base64", commitment: "confirmed"}]
/// - slotSubscribe: []
pub fn build_subscription_params(sub_type: &SubscriptionType) -> serde_json::Value {
    match sub_type {
        SubscriptionType::Program(program_id) => {
            // Program subscription: [programId, options]
            json!([
                program_id.to_string(),
                {
                    "encoding": "base64",
                    "commitment": "confirmed"
                }
            ])
        }
        SubscriptionType::Account(pubkey) => {
            // Account subscription: [accountPubkey, options]
            json!([
                pubkey.to_string(),
                {
                    "encoding": "base64",
                    "commitment": "confirmed"
                }
            ])
        }
        SubscriptionType::Slot => {
            // Slot subscription: []
            json!([])
        }
    }
}

/// Get subscription method name for subscription type
pub fn get_subscription_method(sub_type: &SubscriptionType) -> &'static str {
    match sub_type {
        SubscriptionType::Program(_) => "programSubscribe",  // ✅ FIX: Use programSubscribe for program subscriptions
        SubscriptionType::Account(_) => "accountSubscribe",
        SubscriptionType::Slot => "slotSubscribe",
    }
}

