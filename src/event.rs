use crate::domain::{AccountPosition, LiquidationOpportunity};

#[derive(Debug, Clone)]
pub enum Event {
    AccountUpdated(AccountPosition),
    PotentiallyLiquidatable(LiquidationOpportunity),
    ExecuteLiquidation(LiquidationOpportunity),
    TxResult {
        opportunity: LiquidationOpportunity,
        success: bool,
        signature: Option<String>,
        error: Option<String>,
    },
}
