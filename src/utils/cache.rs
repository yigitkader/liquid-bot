use crate::core::types::Position;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct AccountCache {
    positions: Arc<RwLock<HashMap<Pubkey, Position>>>,
}

impl AccountCache {
    pub fn new() -> Self {
        AccountCache {
            positions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn insert(&self, pubkey: Pubkey, position: Position) {
        self.positions.write().await.insert(pubkey, position);
    }

    pub async fn get(&self, pubkey: &Pubkey) -> Option<Position> {
        self.positions.read().await.get(pubkey).cloned()
    }

    pub async fn update(&self, pubkey: Pubkey, position: Position) {
        self.positions.write().await.insert(pubkey, position);
    }

    pub async fn remove(&self, pubkey: &Pubkey) {
        self.positions.write().await.remove(pubkey);
    }

    pub async fn get_all_liquidatable(&self, threshold: f64) -> Vec<Position> {
        self.positions
            .read()
            .await
            .values()
            .filter(|p| p.health_factor < threshold)
            .cloned()
            .collect()
    }
}
