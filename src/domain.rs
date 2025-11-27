use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountPosition {
    pub account_address: String,
    pub protocol_id: String,
    pub health_factor: f64,
    pub total_collateral_usd: f64,
    pub total_debt_usd: f64,
    pub collateral_assets: Vec<CollateralAsset>,
    pub debt_assets: Vec<DebtAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollateralAsset {
    pub mint: String,
    pub amount: u64,
    pub amount_usd: f64,
    pub ltv: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebtAsset {
    pub mint: String,
    pub amount: u64,
    pub amount_usd: f64,
    pub borrow_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidationOpportunity {
    pub account_position: AccountPosition,
    pub max_liquidatable_amount: u64,
    pub seizable_collateral: u64,
    pub liquidation_bonus: f64,
    pub estimated_profit_usd: f64,
    pub target_debt_mint: String,
    pub target_collateral_mint: String,
}
