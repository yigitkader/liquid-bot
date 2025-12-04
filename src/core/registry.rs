//! Merkezi Registry Modülü
//! 
//! Bu modül projedeki tüm bağımlılıkları (program ID'leri, mint adresleri, 
//! reserve adresleri, IDL dosyaları) merkezi bir yerden yönetir.
//! 
//! Bu sayede:
//! - Hardcoded değerler tek bir yerden yönetilir
//! - Değişiklikler kolayca yapılabilir
//! - Versiyon kontrolü ve güncellemeler daha kolay olur
//! - Kod tekrarı azalır

use anyhow::{Context, Result};
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

/// Program ID'leri için registry
pub struct ProgramIds;

impl ProgramIds {
    /// Solend Program ID (Mainnet)
    pub const SOLEND: &'static str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";
    
    /// Pyth Network Program ID (Mainnet)
    pub const PYTH: &'static str = "FsJ3A3u2vn5cTVofAjvy6y5kwABJAqYWpe4975bi2epH";
    
    /// Switchboard Program ID (Mainnet)
    pub const SWITCHBOARD: &'static str = "SW1TCH7qEPTdLsDHRgPuMQjbQxKdH2aBStViMFnt64f";
    
    /// Associated Token Program ID
    pub const ASSOCIATED_TOKEN: &'static str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
    
    /// Standard SPL Token Program ID (spl_token::id() kullanılabilir ama burada da tutuyoruz)
    pub const TOKEN: &'static str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
    
    /// Token-2022 Program ID (Token Extensions)
    pub const TOKEN_2022: &'static str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
    
    pub fn solend() -> Result<Pubkey> { Self::parse(Self::SOLEND, "Solend program ID") }
    pub fn pyth() -> Result<Pubkey> { Self::parse(Self::PYTH, "Pyth program ID") }
    pub fn switchboard() -> Result<Pubkey> { Self::parse(Self::SWITCHBOARD, "Switchboard program ID") }
    pub fn associated_token() -> Result<Pubkey> { Self::parse(Self::ASSOCIATED_TOKEN, "Associated Token program ID") }
    pub fn token_2022() -> Result<Pubkey> { Self::parse(Self::TOKEN_2022, "Token-2022 program ID") }
    pub fn token() -> Result<Pubkey> { Self::parse(Self::TOKEN, "Token program ID") }
    
    fn parse(addr: &str, name: &str) -> Result<Pubkey> {
        Pubkey::from_str(addr).with_context(|| format!("Failed to parse {}", name))
    }
}

/// Mint adresleri için registry
pub struct MintAddresses;

impl MintAddresses {
    /// USDC Mint (Mainnet)
    pub const USDC: &'static str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    
    /// SOL Mint (Wrapped SOL)
    pub const SOL: &'static str = "So11111111111111111111111111111111111111112";
    
    /// USDT Mint (Mainnet)
    pub const USDT: &'static str = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB";
    
    /// ETH Mint (Wrapped ETH)
    pub const ETH: &'static str = "7vfCXTUXx5WJV5JADk17DUJ4ksgau7utNKj4b963voxs";
    
    /// BTC Mint (Wrapped BTC)
    pub const BTC: &'static str = "9n4nbM75f5Ui33ZbPYXn59EwSgE8CGsHtAeTH5YFeJ9E";
    
    /// DAI Mint
    pub const DAI: &'static str = "EjmyN6qEC1Tf1JxiG1ae7UTJhUxSwk1TCWNWqxWV4J6o";
    
    /// FRAX Mint
    pub const FRAX: &'static str = "FR87nWEUxVgerFGhZM8Y4AggKGLnaXswr1Pd8wZ4kZcp";
    
    /// UST Mint (TerraUSD)
    pub const UST: &'static str = "9vMJfxuKxXBoEa7rM12mYLMwTacLMLDJqHozw96WQL8i";
    
    /// BUSD Mint
    pub const BUSD: &'static str = "AZsHEMXd36Bj1EMNXhowJajpUXzrKcK57wW4ZGXVa7yR";
    
    /// TUSD Mint
    pub const TUSD: &'static str = "4k3Dyjzvzp8eMZWUXbBCjEvwSkkk59S5iCNLY3QrkX6R";
    
    /// USDP Mint (Pax Dollar)
    pub const USDP: &'static str = "EchesyfXePKdLbiHRbgTbYq4qP8zF8LzF6S9X5YJ7KzN";
    
    pub fn usdc() -> Result<Pubkey> { Self::parse(Self::USDC, "USDC mint") }
    pub fn sol() -> Result<Pubkey> { Self::parse(Self::SOL, "SOL mint") }
    pub fn usdt() -> Result<Pubkey> { Self::parse(Self::USDT, "USDT mint") }
    pub fn eth() -> Result<Pubkey> { Self::parse(Self::ETH, "ETH mint") }
    pub fn btc() -> Result<Pubkey> { Self::parse(Self::BTC, "BTC mint") }
    
    fn parse(addr: &str, name: &str) -> Result<Pubkey> {
        Pubkey::from_str(addr).with_context(|| format!("Failed to parse {}", name))
    }
    
    /// Tüm stablecoin mint adreslerini döndürür
    pub fn stablecoins() -> Vec<&'static str> {
        vec![
            Self::USDC,
            Self::USDT,
            Self::DAI,
            Self::FRAX,
            Self::UST,
            Self::BUSD,
            Self::TUSD,
            Self::USDP,
        ]
    }
    
    /// Stablecoin mint adreslerini Pubkey HashSet olarak döndürür
    pub fn stablecoins_as_pubkeys() -> Result<std::collections::HashSet<Pubkey>> {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        for mint_str in Self::stablecoins() {
            let pubkey = Pubkey::from_str(mint_str)
                .with_context(|| format!("Failed to parse stablecoin mint: {}", mint_str))?;
            set.insert(pubkey);
        }
        Ok(set)
    }
}

/// Reserve adresleri için registry
pub struct ReserveAddresses;

impl ReserveAddresses {
    /// USDC Reserve Address (Mainnet)
    pub const USDC: &'static str = "BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw";
    
    /// SOL Reserve Address (Mainnet)
    pub const SOL: &'static str = "8PbodeaosQP19SjYFx855UMqWxH2HynZLdBXmsrbac36";
    
    pub fn usdc() -> Result<Pubkey> { Self::parse(Self::USDC, "USDC reserve") }
    pub fn sol() -> Result<Pubkey> { Self::parse(Self::SOL, "SOL reserve") }
    
    fn parse(addr: &str, name: &str) -> Result<Pubkey> {
        Pubkey::from_str(addr).with_context(|| format!("Failed to parse {}", name))
    }
}

/// Lending Market adresleri için registry
pub struct LendingMarketAddresses;

impl LendingMarketAddresses {
    /// Main Lending Market Address (Mainnet)
    pub const MAIN: &'static str = "4UpD2fh7xH3VP9QQaXtsS1YY3bxzWhtfpks7FatyKvdY";
    
    pub fn main() -> Result<Pubkey> {
        Pubkey::from_str(Self::MAIN).context("Failed to parse main lending market address")
    }
}

/// Pyth Oracle adresleri için registry (Mainnet)
/// 
/// Not: Bu adresler Pyth Network'ün mainnet-beta cluster'ı için geçerlidir.
/// Pyth feed adresleri değişebilir, bu yüzden düzenli olarak güncellenmelidir.
/// Resmi kaynak: https://pyth.network/developers/price-feed-ids
pub struct PythOracleAddresses;

impl PythOracleAddresses {
    /// SOL/USD Price Feed (Mainnet)
    /// Feed ID: 0xef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d
    pub const SOL_USD: &'static str = "H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4AQJEG";
    
    /// USDC/USD Price Feed (Mainnet)
    /// Feed ID: 0xeaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a
    /// Note: This is the correct mainnet address for USDC/USD feed
    pub const USDC_USD: &'static str = "5SSkXsEKQepHHAewytPVwdej4epE1h4EmHtUxJ9rKT98";
    
    /// USDT/USD Price Feed (Mainnet)
    pub const USDT_USD: &'static str = "3vxLXJqLqF3JG5TCbYycbKWRBbCJCMx7E4xrTU5XG8Jz";
    
    /// ETH/USD Price Feed (Mainnet)
    pub const ETH_USD: &'static str = "JBu1AL4obBcCMqKBBxhpWCNUt136ijcuMZLFvTP7iWdB";
    
    /// BTC/USD Price Feed (Mainnet)
    pub const BTC_USD: &'static str = "GVXRSBjFk6e6J3NbVPXohDJetcTjaeeuykUpbQF8UoMU";
    
    pub fn sol_usd() -> Result<Pubkey> { Self::parse(Self::SOL_USD, "SOL/USD Pyth oracle") }
    pub fn usdc_usd() -> Result<Pubkey> { Self::parse(Self::USDC_USD, "USDC/USD Pyth oracle") }
    pub fn usdt_usd() -> Result<Pubkey> { Self::parse(Self::USDT_USD, "USDT/USD Pyth oracle") }
    pub fn eth_usd() -> Result<Pubkey> { Self::parse(Self::ETH_USD, "ETH/USD Pyth oracle") }
    pub fn btc_usd() -> Result<Pubkey> { Self::parse(Self::BTC_USD, "BTC/USD Pyth oracle") }
    
    fn parse(addr: &str, name: &str) -> Result<Pubkey> {
        Pubkey::from_str(addr).with_context(|| format!("Failed to parse {}", name))
    }
    
    /// Mint adresine göre Pyth oracle adresini döndürür
    pub fn get_oracle_for_mint(mint: &Pubkey) -> Result<Option<Pubkey>> {
        use crate::core::registry::MintAddresses;
        
        let sol_mint = MintAddresses::sol()?;
        let usdc_mint = MintAddresses::usdc()?;
        let usdt_mint = MintAddresses::usdt()?;
        let eth_mint = MintAddresses::eth()?;
        let btc_mint = MintAddresses::btc()?;
        
        Ok(if *mint == sol_mint {
            Some(Self::sol_usd()?)
        } else if *mint == usdc_mint {
            Some(Self::usdc_usd()?)
        } else if *mint == usdt_mint {
            Some(Self::usdt_usd()?)
        } else if *mint == eth_mint {
            Some(Self::eth_usd()?)
        } else if *mint == btc_mint {
            Some(Self::btc_usd()?)
        } else {
            None
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_program_ids_parse() {
        assert!(ProgramIds::solend().is_ok());
        assert!(ProgramIds::pyth().is_ok());
        assert!(ProgramIds::switchboard().is_ok());
        assert!(ProgramIds::associated_token().is_ok());
        assert!(ProgramIds::token_2022().is_ok());
    }
    
    #[test]
    fn test_mint_addresses_parse() {
        assert!(MintAddresses::usdc().is_ok());
        assert!(MintAddresses::sol().is_ok());
        assert!(MintAddresses::usdt().is_ok());
        assert!(MintAddresses::eth().is_ok());
        assert!(MintAddresses::btc().is_ok());
    }
    
    #[test]
    fn test_reserve_addresses_parse() {
        assert!(ReserveAddresses::usdc().is_ok());
        assert!(ReserveAddresses::sol().is_ok());
    }
    
    #[test]
    fn test_lending_market_addresses_parse() {
        assert!(LendingMarketAddresses::main().is_ok());
    }
    
    #[test]
    fn test_stablecoins_parse() {
        let stablecoins = MintAddresses::stablecoins_as_pubkeys().unwrap();
        assert!(!stablecoins.is_empty());
        assert!(stablecoins.contains(&MintAddresses::usdc().unwrap()));
        assert!(stablecoins.contains(&MintAddresses::usdt().unwrap()));
    }
}

