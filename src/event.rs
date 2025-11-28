use crate::domain::{AccountPosition, LiquidationOpportunity};
use solana_sdk::pubkey::Pubkey;

#[derive(Debug, Clone)]
pub enum Event {
    // WebSocket-driven events (event-driven, real-time)
    AccountUpdated(AccountPosition),
    PriceUpdate {
        mint: String,
        price: f64,
        confidence: f64,
        timestamp: i64,
    },
    SlotUpdate {
        slot: u64,
    },
    // RPC-driven events (state reading, calculation, transaction)
    AccountCheckRequest {
        account_address: String,
        reason: String, // e.g., "price_update", "slot_update", "program_notification"
    },
    // Liquidation pipeline events
    PotentiallyLiquidatable(LiquidationOpportunity),
    ExecuteLiquidation(LiquidationOpportunity),
    TxResult {
        opportunity: LiquidationOpportunity,
        success: bool,
        signature: Option<String>,
        error: Option<String>,
    },
}
