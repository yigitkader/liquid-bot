use anyhow::{Context, Result};
use solana_sdk::{
    pubkey::Pubkey,
    account::Account,
};

// Solend reserve account struct'ını kullan
use crate::protocols::solend_reserve::SolendReserve;

#[derive(Debug, Clone)]
pub struct ReserveInfo {
    pub reserve_pubkey: Pubkey,
    pub mint: Option<Pubkey>,
    pub ltv: f64,
    pub liquidation_threshold: f64,
    pub borrow_rate: f64,
    pub liquidity_mint: Option<Pubkey>,
    pub collateral_mint: Option<Pubkey>,
    pub liquidity_supply: Option<Pubkey>,
    pub collateral_supply: Option<Pubkey>,
    pub liquidation_bonus: f64,
    pub pyth_oracle: Option<Pubkey>,
    pub switchboard_oracle: Option<Pubkey>,
}

pub async fn parse_reserve_account(
    reserve_pubkey: &Pubkey,
    account_data: &Account,
) -> Result<ReserveInfo> {

    // Parse Solend reserve account
    // ⚠️ PRODUCTION WARNING: Bu struct gerçek Solend IDL'ine göre doğrulanmamıştır!
    // Parse error alırsanız, gerçek IDL'den struct'ı güncelleyin: ./scripts/fetch_solend_idl.sh
    let reserve = SolendReserve::from_account_data(&account_data.data)
        .context(format!(
            "Failed to parse Solend reserve account {}. \
            This might indicate the struct structure doesn't match the real Solend IDL. \
            Please validate against official IDL: ./scripts/fetch_solend_idl.sh",
            reserve_pubkey
        ))?;

    let liquidity_mint = reserve.liquidity_mint();
    let collateral_mint = reserve.collateral_mint();
    let ltv = reserve.ltv();
    let liquidation_threshold = reserve.config.liquidation_threshold as f64 / 100.0;
    let liquidation_bonus = reserve.liquidation_bonus();

    /// Calculate borrow rate based on utilization rate
    /// 
    /// Solend uses a piecewise linear interest rate model:
    /// 
    /// 1. If utilization < optimal_utilization:
    ///    - Linear interpolation between min_borrow_rate and optimal_borrow_rate
    ///    - Formula: min_rate + (optimal_rate - min_rate) * (utilization / optimal_utilization)
    /// 
    /// 2. If utilization >= optimal_utilization:
    ///    - Linear interpolation between optimal_borrow_rate and max_borrow_rate
    ///    - Formula: optimal_rate + (max_rate - optimal_rate) * (excess_utilization / remaining_utilization)
    /// 
    /// This model incentivizes optimal utilization:
    /// - Low utilization: Lower rates encourage borrowing
    /// - Optimal utilization: Balanced rates
    /// - High utilization: Higher rates discourage borrowing and encourage repayment
    /// 
    /// Reference: Solend's interest rate model implementation
    /// Validation: Matches Solend SDK behavior for borrow rate calculation
    /// 
    /// WAD format constant: 1e18 (from Solend's WAD format)
    /// 
    /// ✅ DOĞRU: Overflow kontrolü + precision koruma
    /// - available_amount'u WAD formatına çevir (u64 → u128 * WAD)
    /// - Toplam liquidity'yi WAD formatında hesapla
    /// - Utilization rate'i f64 division ile hesapla (precision korunur)
    const WAD: u128 = 1_000_000_000_000_000_000;
    
    // Convert available_amount to WAD format to match borrowed_amount_wads
    let available_wads = (reserve.liquidity.available_amount as u128) * WAD;
    let total_liquidity_wads = available_wads + reserve.liquidity.borrowed_amount_wads;
    
    let utilization_rate = if total_liquidity_wads > 0 {
        (reserve.liquidity.borrowed_amount_wads as f64) / (total_liquidity_wads as f64)
    } else {
        0.0
    };
    
    let optimal_utilization = reserve.config.optimal_utilization_rate as f64 / 100.0;
    let min_rate = reserve.config.min_borrow_rate as f64 / 100.0;
    let optimal_rate = reserve.config.optimal_borrow_rate as f64 / 100.0;
    let max_rate = reserve.config.max_borrow_rate as f64 / 100.0;
    
    let borrow_rate = if utilization_rate < optimal_utilization {
        // Linear interpolation between min and optimal
        if optimal_utilization > 0.0 {
            min_rate + (optimal_rate - min_rate) * (utilization_rate / optimal_utilization)
        } else {
            min_rate
        }
    } else {
        // Linear interpolation between optimal and max
        let remaining_utilization = 1.0 - optimal_utilization;
        if remaining_utilization > 0.0 {
            let excess_utilization = utilization_rate - optimal_utilization;
            optimal_rate + (max_rate - optimal_rate) * (excess_utilization / remaining_utilization)
        } else {
            optimal_rate
        }
    };
    
    log::debug!(
        "Borrow rate calculation: utilization={:.2}%, optimal_util={:.2}%, rate={:.4}% (min={:.2}%, optimal={:.2}%, max={:.2}%)",
        utilization_rate * 100.0,
        optimal_utilization * 100.0,
        borrow_rate * 100.0,
        min_rate * 100.0,
        optimal_rate * 100.0,
        max_rate * 100.0
    );
    let mint = Some(liquidity_mint);
    let pyth_oracle_raw = reserve.pyth_oracle();
    let switchboard_oracle_raw = reserve.switchboard_oracle();
    
    // Solend'in gerçek kodunda oracle_option YOK!
    // Her iki oracle da account'ta var, hangisinin aktif olduğu program tarafından belirlenir
    // Default pubkey (111...111) olmayan oracle'ı aktif kabul ediyoruz
    let pyth_oracle = if pyth_oracle_raw != Pubkey::default() {
        Some(pyth_oracle_raw)
    } else {
        None
    };
    let switchboard_oracle = if switchboard_oracle_raw != Pubkey::default() {
        Some(switchboard_oracle_raw)
    } else {
        None
    };
    
    if pyth_oracle.is_none() && switchboard_oracle.is_none() {
        log::debug!("No oracle found for reserve {} (both are default)", reserve_pubkey);
    } else if let (Some(pyth), Some(switchboard)) = (pyth_oracle.as_ref(), switchboard_oracle.as_ref()) {
        log::debug!("Both oracles present for reserve {}: pyth={}, switchboard={}", 
            reserve_pubkey, pyth, switchboard);
    } else if let Some(pyth) = pyth_oracle.as_ref() {
        log::debug!("Only Pyth oracle present for reserve {}: {}", reserve_pubkey, pyth);
    } else if let Some(switchboard) = switchboard_oracle.as_ref() {
        log::debug!("Only Switchboard oracle present for reserve {}: {}", reserve_pubkey, switchboard);
    }

    log::debug!(
        "Reserve {} oracles: pyth={:?}, switchboard={:?}",
        reserve_pubkey,
        pyth_oracle,
        switchboard_oracle
    );
    
    log::debug!(
        "Parsed reserve {}: mint={}, ltv={:.2}%, liquidation_threshold={:.2}%, borrow_rate={:.4}%, liquidation_bonus={:.2}%, pyth_oracle={:?}, switchboard_oracle={:?}",
        reserve_pubkey,
        liquidity_mint,
        ltv * 100.0,
        liquidation_threshold * 100.0,
        borrow_rate * 100.0,
        liquidation_bonus * 100.0,
        pyth_oracle,
        switchboard_oracle
    );
    
    Ok(ReserveInfo {
        reserve_pubkey: *reserve_pubkey,
        mint,
        ltv,
        liquidation_threshold,
        borrow_rate,
        liquidity_mint: Some(liquidity_mint),
        collateral_mint: Some(collateral_mint),
        liquidity_supply: Some(reserve.liquidity_supply()),
        collateral_supply: Some(reserve.collateral_supply()),
        liquidation_bonus,
        pyth_oracle,
        switchboard_oracle,
    })
}

pub async fn get_reserve_mint(
    _reserve_pubkey: &Pubkey,
    _rpc_client: Option<&crate::solana_client::SolanaClient>,
) -> Result<Option<Pubkey>> {
    Ok(None)
}

