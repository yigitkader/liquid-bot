use async_trait::async_trait;
use anyhow::{Context, Result};
use solana_sdk::{
    pubkey::Pubkey,
    instruction::{Instruction, AccountMeta},
    account::Account,
};
use crate::domain::AccountPosition;
use crate::protocol::{Protocol, LiquidationParams};
use crate::solana_client::SolanaClient;
use sha2::{Sha256, Digest};
use std::sync::Arc;

// Solend account helper functions
mod solend_accounts {
    use anyhow::Result;
    use solana_sdk::pubkey::Pubkey;
    use std::str::FromStr;
    
    /// Associated Token Account (ATA) adresini hesaplar
    pub fn get_associated_token_address(
        wallet: &Pubkey,
        mint: &Pubkey,
    ) -> Result<Pubkey> {
        let associated_token_program_id = Pubkey::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL")
            .map_err(|_| anyhow::anyhow!("Invalid associated token program ID"))?;
        
        let token_program_id = spl_token::id();
        let seeds = &[
            wallet.as_ref(),
            token_program_id.as_ref(),
            mint.as_ref(),
        ];
        
        Pubkey::try_find_program_address(seeds, &associated_token_program_id)
            .map(|(pubkey, _)| pubkey)
            .ok_or_else(|| anyhow::anyhow!("Failed to derive associated token address"))
    }
    
    /// Lending Market Authority PDA'sını hesaplar
    pub fn derive_lending_market_authority(
        lending_market: &Pubkey,
        program_id: &Pubkey,
    ) -> Result<Pubkey> {
        let seeds = &[
            b"lending-market-authority".as_ref(),
            lending_market.as_ref(),
        ];
        
        Pubkey::try_find_program_address(seeds, program_id)
            .map(|(pubkey, _)| pubkey)
            .ok_or_else(|| anyhow::anyhow!("Failed to derive lending market authority"))
    }
}

use solend_accounts::{get_associated_token_address, derive_lending_market_authority};

// Solend IDL account structures
mod solend_idl {
    use borsh::{BorshDeserialize, BorshSerialize};
    use solana_sdk::pubkey::Pubkey;

    /// Solend Obligation Account yapısı (IDL'den)
    /// Gerçek Solend obligation account yapısı
    #[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
    pub struct SolendObligation {
        pub last_update_slot: u64,
        pub lending_market: Pubkey,
        pub owner: Pubkey,
        pub deposited_value: Number,
        pub borrowed_value: Number,
        pub allowed_borrow_value: Number,
        pub unhealthy_borrow_value: Number,
        pub deposits: Vec<ObligationCollateral>,
        pub borrows: Vec<ObligationLiquidity>,
    }

    /// Obligation Collateral (deposit)
    #[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
    pub struct ObligationCollateral {
        pub deposit_reserve: Pubkey,
        pub deposited_amount: u64,
        pub market_value: Number,
    }

    /// Obligation Liquidity (borrow)
    #[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
    pub struct ObligationLiquidity {
        pub borrow_reserve: Pubkey,
        pub cumulative_borrow_rate_wad: u128,
        pub borrowed_amount_wad: u128,
        pub market_value: Number,
    }

    /// Number wrapper (u128)
    #[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
    pub struct Number {
        pub value: u128,
    }

    impl Number {
        pub fn to_f64(&self) -> f64 {
            // Solend'de genellikle WAD (1e18) formatında tutulur
            self.value as f64 / 1_000_000_000_000_000_000.0
        }
        
        pub fn to_u64(&self) -> u64 {
            // WAD'dan normal değere çevir
            (self.value / 1_000_000_000_000_000_000) as u64
        }
    }

    impl SolendObligation {
        /// Account data'dan obligation parse eder (Anchor discriminator ile)
        pub fn from_account_data(data: &[u8]) -> anyhow::Result<Self> {
            if data.is_empty() {
                return Err(anyhow::anyhow!("Empty account data"));
            }
            
            // Anchor programlarında ilk 8 byte discriminator'dır
            // Obligation account için discriminator: [0x6f, 0x62, 0x6c, 0x69, 0x67, 0x61, 0x74, 0x69] ("obligati")
            // Ancak genel olarak ilk 8 byte'ı atlayabiliriz
            let account_data = if data.len() > 8 {
                &data[8..]
            } else {
                data
            };
            
            // Borsh deserialize
            SolendObligation::try_from_slice(account_data)
                .map_err(|e| anyhow::anyhow!("Failed to deserialize Solend obligation: {}", e))
        }
        
        /// Total deposited value (USD cinsinden)
        pub fn total_deposited_value_usd(&self) -> f64 {
            self.deposited_value.to_f64()
        }
        
        /// Total borrowed value (USD cinsinden)
        pub fn total_borrowed_value_usd(&self) -> f64 {
            self.borrowed_value.to_f64()
        }
        
        /// Health factor hesaplar
        /// HF = deposited_value / borrowed_value
        pub fn calculate_health_factor(&self) -> f64 {
            let borrowed = self.total_borrowed_value_usd();
            if borrowed == 0.0 {
                return f64::INFINITY;
            }
            
            let deposited = self.total_deposited_value_usd();
            deposited / borrowed
        }
    }
}

use solend_idl::{SolendObligation, ObligationCollateral, ObligationLiquidity};

/// Solend Protocol implementasyonu
pub struct SolendProtocol {
    program_id: Pubkey,
}

impl SolendProtocol {
    /// Solend Mainnet Program ID
    pub const SOLEND_PROGRAM_ID: &'static str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";
    
    pub fn new() -> Result<Self> {
        let program_id = Pubkey::try_from(Self::SOLEND_PROGRAM_ID)
            .context("Invalid Solend program ID")?;
        
        Ok(SolendProtocol { program_id })
    }
    
    /// Liquidation instruction için gerekli account'ları resolve eder
    /// 
    /// Bu fonksiyon:
    /// 1. Obligation account'unu RPC'den okur
    /// 2. Reserve account'unu bulur ve okur
    /// 3. Token account'larını hesaplar
    /// 4. PDA'ları hesaplar
    /// 5. Oracle account'larını bulur
    async fn resolve_liquidation_accounts(
        &self,
        opportunity: &crate::domain::LiquidationOpportunity,
        liquidator: &Pubkey,
        rpc_client: &SolanaClient,
    ) -> Result<(
        Pubkey, // source_liquidity
        Pubkey, // destination_collateral
        Pubkey, // reserve
        Pubkey, // reserve_collateral_mint
        Pubkey, // reserve_liquidity_supply
        Pubkey, // lending_market_authority
        Pubkey, // destination_liquidity
        Pubkey, // pyth_price
        Pubkey, // switchboard_price
    )> {
        // 1. Obligation account'unu oku
        let obligation_pubkey = Pubkey::try_from(opportunity.account_position.account_address.as_str())
            .context("Invalid obligation pubkey")?;
        
        let obligation_account = rpc_client.get_account(&obligation_pubkey).await
            .context("Failed to fetch obligation account")?;
        
        // 2. Obligation'ı parse et
        let obligation = solend_idl::SolendObligation::from_account_data(&obligation_account.data)
            .context("Failed to parse obligation account")?;
        
        // 3. Borrow reserve'ü bul (target_debt_mint'ten veya obligation.borrows'den)
        // target_debt_mint reserve pubkey olabilir veya mint address olabilir
        let borrow_reserve_pubkey = if let Ok(reserve_pubkey) = Pubkey::try_from(opportunity.target_debt_mint.as_str()) {
            // target_debt_mint bir pubkey ise direkt kullan
            reserve_pubkey
        } else {
            // target_debt_mint bir mint address ise, obligation.borrows'den reserve'i bul
            obligation.borrows.first()
                .map(|b| b.borrow_reserve)
                .ok_or_else(|| anyhow::anyhow!("No borrow found in obligation"))?
        };
        
        // 4. Reserve account'unu oku
        let reserve_account = rpc_client.get_account(&borrow_reserve_pubkey).await
            .context("Failed to fetch reserve account")?;
        
        // 5. Reserve account'unu parse et (gerçek implementasyon)
        use crate::protocols::reserve_helper::parse_reserve_account;
        let reserve_info = parse_reserve_account(&borrow_reserve_pubkey, &reserve_account).await
            .context("Failed to parse reserve account")?;
        
        // Reserve'den gerçek mint ve token account'ları al
        let debt_mint = reserve_info.liquidity_mint
            .ok_or_else(|| anyhow::anyhow!("Reserve liquidity mint not found"))?;
        let reserve_liquidity_supply = reserve_info.liquidity_supply
            .ok_or_else(|| anyhow::anyhow!("Reserve liquidity supply not found"))?;
        
        // 6. Collateral reserve'ü bul (target_collateral_mint'ten)
        let collateral_reserve_pubkey = if let Ok(reserve_pubkey) = Pubkey::try_from(opportunity.target_collateral_mint.as_str()) {
            reserve_pubkey
        } else {
            obligation.deposits.first()
                .map(|d| d.deposit_reserve)
                .ok_or_else(|| anyhow::anyhow!("No deposit found in obligation"))?
        };
        
        // 7. Collateral reserve account'unu oku ve parse et
        let collateral_reserve_account = rpc_client.get_account(&collateral_reserve_pubkey).await
            .context("Failed to fetch collateral reserve account")?;
        let collateral_reserve_info = parse_reserve_account(&collateral_reserve_pubkey, &collateral_reserve_account).await
            .context("Failed to parse collateral reserve account")?;
        
        // Collateral reserve'den gerçek mint ve token account'ları al
        let collateral_mint = collateral_reserve_info.collateral_mint
            .ok_or_else(|| anyhow::anyhow!("Collateral reserve mint not found"))?;
        let reserve_collateral_mint = collateral_mint; // Collateral mint = reserve collateral mint
        let destination_liquidity = reserve_liquidity_supply; // Destination liquidity = reserve liquidity supply
        
        // 8. Lending market authority PDA'sını hesapla
        let lending_market_authority = derive_lending_market_authority(&self.program_id, &self.program_id)
            .context("Failed to derive lending market authority")?;
        
        // 9. Liquidator'ın token account'larını hesapla (ATA)
        // Gerçek mint address'leri artık reserve'den alınıyor ✅
        let source_liquidity = get_associated_token_address(liquidator, &debt_mint)
            .context("Failed to derive source liquidity ATA")?;
        let destination_collateral = get_associated_token_address(liquidator, &collateral_mint)
            .context("Failed to derive destination collateral ATA")?;
        
        // 9. Oracle account'ları (mint'ten türet)
        // Solend'de oracle account'ları genellikle mint address'inden türetilir
        use crate::protocols::oracle_helper::get_oracle_accounts;
        let (pyth_price, switchboard_price) = get_oracle_accounts(&debt_mint)
            .context("Failed to get oracle accounts")?;
        
        // Oracle account'ları bulunamazsa fallback (optional account olabilir)
        let pyth_price = pyth_price.unwrap_or_else(|| {
            log::warn!("Pyth oracle not found for mint {}, using default", debt_mint);
            Pubkey::default()
        });
        let switchboard_price = switchboard_price.unwrap_or_else(|| {
            log::warn!("Switchboard oracle not found for mint {}, using default", debt_mint);
            Pubkey::default()
        });
        
        log::debug!(
            "Resolved liquidation accounts: reserve={}, source_liquidity={}, destination_collateral={}",
            borrow_reserve_pubkey,
            source_liquidity,
            destination_collateral
        );
        
        Ok((
            source_liquidity,
            destination_collateral,
            borrow_reserve_pubkey, // reserve
            reserve_collateral_mint,
            reserve_liquidity_supply,
            lending_market_authority,
            destination_liquidity,
            pyth_price,
            switchboard_price,
        ))
    }
}

#[async_trait]
impl Protocol for SolendProtocol {
    fn id(&self) -> &str {
        "Solend"
    }
    
    fn program_id(&self) -> Pubkey {
        self.program_id
    }
    
    async fn parse_account_position(
        &self,
        account_address: &Pubkey,
        account_data: &Account,
        rpc_client: Option<Arc<SolanaClient>>,
    ) -> Result<Option<AccountPosition>> {
        log::debug!("Parsing Solend account: {}", account_address);
        
        // Account'un bu program'a ait olduğunu kontrol et
        if account_data.owner != self.program_id {
            return Ok(None);
        }
        
        // Account data'yı parse et
        let obligation = match SolendObligation::from_account_data(&account_data.data) {
            Ok(obligation) => obligation,
            Err(e) => {
                log::warn!("Failed to parse Solend obligation {}: {}", account_address, e);
                return Ok(None);
            }
        };
        
        // Health factor hesapla
        let health_factor = obligation.calculate_health_factor();
        
        // Gerçek Solend obligation account yapısından değerleri çıkar
        let total_collateral_usd = obligation.total_deposited_value_usd();
        let total_debt_usd = obligation.total_borrowed_value_usd();
        
        // Collateral assets'i domain modeline dönüştür
        // Gerçek mint address'lerini reserve account'larından al
        use crate::protocols::reserve_helper::parse_reserve_account;
        
        let mut collateral_assets = Vec::new();
        for deposit in &obligation.deposits {
            // RPC client varsa gerçek reserve account'unu parse et
            let (mint, ltv) = if let Some(rpc) = &rpc_client {
                match rpc.get_account(&deposit.deposit_reserve).await {
                    Ok(reserve_account) => {
                        match parse_reserve_account(&deposit.deposit_reserve, &reserve_account).await {
                            Ok(reserve_info) => {
                                // Gerçek mint ve LTV değerlerini kullan
                                let mint = reserve_info.collateral_mint
                                    .unwrap_or_else(|| reserve_info.liquidity_mint.unwrap_or(deposit.deposit_reserve));
                                (mint.to_string(), reserve_info.ltv)
                            }
                            Err(e) => {
                                log::warn!("Failed to parse reserve {}: {}, using fallback", deposit.deposit_reserve, e);
                                // Fallback: Reserve pubkey'i mint olarak kullan
                                (deposit.deposit_reserve.to_string(), 0.75)
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("Failed to fetch reserve {}: {}, using fallback", deposit.deposit_reserve, e);
                        // Fallback: Reserve pubkey'i mint olarak kullan
                        (deposit.deposit_reserve.to_string(), 0.75)
                    }
                }
            } else {
                // RPC client yoksa fallback: Reserve pubkey'i mint olarak kullan
                (deposit.deposit_reserve.to_string(), 0.75)
            };
            
            collateral_assets.push(crate::domain::CollateralAsset {
                mint,
                amount: deposit.deposited_amount,
                amount_usd: deposit.market_value.to_f64(),
                ltv,
            });
        }
        
        // Debt assets'i domain modeline dönüştür
        // Gerçek mint address'lerini reserve account'larından al
        let mut debt_assets = Vec::new();
        for borrow in &obligation.borrows {
            // Borrowed amount'u WAD'dan normal değere çevir
            // WAD = 1e18 (Wei Adjusted Decimal)
            let borrowed_amount = (borrow.borrowed_amount_wad / 1_000_000_000_000_000_000) as u64;
            
            // RPC client varsa gerçek reserve account'unu parse et
            let (mint, borrow_rate) = if let Some(rpc) = &rpc_client {
                match rpc.get_account(&borrow.borrow_reserve).await {
                    Ok(reserve_account) => {
                        match parse_reserve_account(&borrow.borrow_reserve, &reserve_account).await {
                            Ok(reserve_info) => {
                                // Gerçek mint ve borrow rate değerlerini kullan
                                let mint = reserve_info.liquidity_mint
                                    .unwrap_or(borrow.borrow_reserve);
                                (mint.to_string(), reserve_info.borrow_rate)
                            }
                            Err(e) => {
                                log::warn!("Failed to parse reserve {}: {}, using fallback", borrow.borrow_reserve, e);
                                // Fallback: Reserve pubkey'i mint olarak kullan
                                (borrow.borrow_reserve.to_string(), 0.0)
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("Failed to fetch reserve {}: {}, using fallback", borrow.borrow_reserve, e);
                        // Fallback: Reserve pubkey'i mint olarak kullan
                        (borrow.borrow_reserve.to_string(), 0.0)
                    }
                }
            } else {
                // RPC client yoksa fallback: Reserve pubkey'i mint olarak kullan
                (borrow.borrow_reserve.to_string(), 0.0)
            };
            
            debt_assets.push(crate::domain::DebtAsset {
                mint,
                amount: borrowed_amount,
                amount_usd: borrow.market_value.to_f64(),
                borrow_rate,
            });
        }
        
        log::debug!(
            "Parsed Solend obligation: account={}, hf={:.4}, collateral=${:.2}, debt=${:.2}",
            account_address,
            health_factor,
            total_collateral_usd,
            total_debt_usd
        );
        
        let position = AccountPosition {
            account_address: account_address.to_string(),
            protocol_id: self.id().to_string(),
            health_factor,
            total_collateral_usd,
            total_debt_usd,
            collateral_assets,
            debt_assets,
        };
        
        Ok(Some(position))
    }
    
    fn calculate_health_factor(&self, position: &AccountPosition) -> Result<f64> {
        // Eğer position'da zaten health_factor varsa onu kullan
        if position.health_factor > 0.0 {
            return Ok(position.health_factor);
        }
        
        // Aksi halde hesapla (basit formül - gerçekte protokol formülüne göre olmalı)
        if position.total_debt_usd == 0.0 {
            return Ok(f64::INFINITY);
        }
        
        // Solend'in health factor formülü (basitleştirilmiş)
        // Gerçek implementasyonda protokolün kendi formülünü kullan
        let collateral_value = position.total_collateral_usd;
        let debt_value = position.total_debt_usd;
        
        // Health Factor = (Collateral * LTV) / Debt
        // Basit bir yaklaşım - gerçekte her asset için ayrı LTV kullanılır
        Ok((collateral_value * 0.75) / debt_value) // %75 LTV varsayımı
    }
    
    fn get_liquidation_params(&self) -> LiquidationParams {
        LiquidationParams {
            liquidation_bonus: 0.05,  // %5 bonus
            close_factor: 0.5,         // %50'ye kadar likide edilebilir
            max_liquidation_slippage: 0.01, // %1 slippage
        }
    }
    
    async fn build_liquidation_instruction(
        &self,
        opportunity: &crate::domain::LiquidationOpportunity,
        liquidator: &Pubkey,
        rpc_client: Option<Arc<SolanaClient>>,
    ) -> Result<Instruction> {
        // Solend liquidation instruction oluşturma (IDL'ye göre)
        // IDL'den: liquidateObligation instruction
        
        let obligation_pubkey = Pubkey::try_from(opportunity.account_position.account_address.as_str())
            .context("Invalid obligation pubkey")?;
        
        // Anchor instruction discriminator: sha256("global:liquidateObligation")[0..8]
        // Anchor'da instruction discriminator = sha256("global:<instruction_name>")[0..8]
        let mut hasher = Sha256::new();
        hasher.update(b"global:liquidateObligation");
        let hash = hasher.finalize();
        let instruction_discriminator: [u8; 8] = hash[0..8].try_into()
            .map_err(|_| anyhow::anyhow!("Failed to create discriminator"))?;
        
        // IDL'ye göre account listesi (liquidateObligation instruction)
        // 
        // Gerçek account'ları almak için:
        // 1. Obligation account'unu RPC'den oku ve parse et
        // 2. Obligation'daki borrow reserve'ü bul (target_debt_mint'ten)
        // 3. Reserve account'unu RPC'den oku ve parse et
        // 4. Reserve'den gerekli account'ları al
        // 5. Liquidator'ın token account'larını hesapla (ATA)
        // 6. Lending market authority PDA'sını hesapla
        // 7. Oracle account'larını bul
        
        let (source_liquidity, destination_collateral, reserve, reserve_collateral_mint, 
             reserve_liquidity_supply, lending_market_authority, destination_liquidity,
             pyth_price, switchboard_price) = if let Some(rpc) = rpc_client {
            // Gerçek account'ları al
            self.resolve_liquidation_accounts(opportunity, liquidator, &*rpc).await?
        } else {
            // RPC client yoksa placeholder kullan (dry-run veya test için)
            log::warn!("⚠️  RPC client not provided, using placeholder accounts. Real transaction will fail!");
            (
                Pubkey::default(), // sourceLiquidity
                Pubkey::default(), // destinationCollateral
                Pubkey::default(), // reserve
                Pubkey::default(), // reserveCollateralMint
                Pubkey::default(), // reserveLiquiditySupply
                Pubkey::default(), // lendingMarketAuthority
                Pubkey::default(), // destinationLiquidity
                Pubkey::default(), // pythPrice
                Pubkey::default(), // switchboardPrice
            )
        };
        
        // IDL sırasına göre account listesi
        let accounts = vec![
            AccountMeta::new_readonly(source_liquidity, false),        // sourceLiquidity (liquidator'ın debt token account'u)
            AccountMeta::new(destination_collateral, false),          // destinationCollateral (liquidator'ın collateral token account'u)
            AccountMeta::new(obligation_pubkey, false),               // obligation
            AccountMeta::new(reserve, false),                          // reserve
            AccountMeta::new(reserve_collateral_mint, false),         // reserveCollateralMint
            AccountMeta::new(reserve_liquidity_supply, false),        // reserveLiquiditySupply
            AccountMeta::new_readonly(self.program_id, false),        // lendingMarket
            AccountMeta::new_readonly(lending_market_authority, false), // lendingMarketAuthority (PDA)
            AccountMeta::new(destination_liquidity, false),          // destinationLiquidity (reserve'e geri gönderilecek)
            AccountMeta::new(*liquidator, true),                      // liquidator (signer)
            AccountMeta::new_readonly(pyth_price, false),             // pythPrice (oracle)
            AccountMeta::new_readonly(switchboard_price, false),      // switchboardPrice (oracle)
            AccountMeta::new_readonly(spl_token::id(), false),         // tokenProgram
        ];
        
        // Instruction data: [discriminator (8 bytes), liquidityAmount (8 bytes)]
        let mut data = Vec::with_capacity(16);
        data.extend_from_slice(&instruction_discriminator);
        data.extend_from_slice(&opportunity.max_liquidatable_amount.to_le_bytes());
        
        log::debug!(
            "Building Solend liquidation instruction: obligation={}, amount={}",
            obligation_pubkey,
            opportunity.max_liquidatable_amount
        );
        
        // Gelecek İyileştirme: Reserve Account Parsing
        // 
        // Gerçek implementasyonda reserve account'larını almak için:
        // 1. Obligation account'u parse et (zaten yapılıyor) ✅
        // 2. Borrow reserve'ü bul (opportunity.target_debt_mint veya obligation.borrows'den)
        // 3. Reserve account'unu RPC'den oku
        // 4. Reserve account'unu parse et (reserve_helper modülü ile)
        // 5. Reserve'den gerekli account'ları al:
        //    - liquidity_supply (token account)
        //    - collateral_mint
        //    - liquidity_mint
        //    - oracle_account (Pyth veya Switchboard)
        // 6. Lending market authority'yi hesapla (PDA derivation)
        // 7. Liquidator'ın source/destination accounts'larını hesapla
        // 8. Tüm account'ları IDL sırasına göre ekle
        //
        // Gerekli modüller:
        // - ✅ reserve_helper.rs (hazırlandı, placeholder)
        // - ⏳ Reserve account struct (IDL'den)
        // - ⏳ Oracle account bulma logic'i
        // - ⏳ PDA derivation helper
        
        Ok(Instruction {
            program_id: self.program_id,
            accounts,
            data,
        })
    }
}

