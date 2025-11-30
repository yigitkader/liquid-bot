# Liquidation Bot Analizi ve Yeni Yapı Tasarımı

Mevcut kodu detaylı inceledim. Şimdi size:
1. Gerekli API'ler ve fonksiyonlar
2. Yeni yapı tasarımı (pseudo-kod ile)
3. Daha temiz bir sistem önerisi

sunacağım.

---

## 1. GEREKLİ API'LER VE FONKSİYONLAR

### A) Solana RPC API'leri
```
CORE RPC CALLS:
- getAccountInfo(pubkey) → account data
- getProgramAccounts(programId, filters?) → [(pubkey, account)]
- getRecentBlockhash() → blockhash
- sendTransaction(tx) → signature
- getSlot() → current slot

OPTIONAL (for optimization):
- getMultipleAccounts([pubkeys]) → [account]
- simulateTransaction(tx) → simulation result
```

### B) Solana WebSocket API'leri
```
SUBSCRIPTIONS:
- programSubscribe(programId, options) → subscription_id
  → notifications: {pubkey, account, slot}
- accountSubscribe(pubkey, options) → subscription_id
  → notifications: {account, slot}
- slotSubscribe() → subscription_id
  → notifications: {slot, parent, root}
```

### C) Oracle API'leri
```
PYTH:
- Read from account data → {price, confidence, expo, timestamp}
- Parse SolanaPriceAccount struct

SWITCHBOARD:
- Read from account data → {price, confidence, timestamp}
```

### D) Jupiter Swap API
```
GET /quote:
  params: {inputMint, outputMint, amount, slippageBps}
  returns: {priceImpactPct, route, ...}

Used for: Real-time slippage estimation
```

### E) Protocol-Specific (Solend)
```
ACCOUNT PARSING:
- Obligation: borsh deserialize → {deposits, borrows, health}
- Reserve: borsh deserialize → {liquidity, collateral, config}

PDA DERIVATION:
- derive_lending_market_authority(market, program)
- derive_obligation_address(wallet, market, program)
- get_associated_token_address(wallet, mint)

INSTRUCTION BUILDING:
- liquidateObligation(discriminator, amount, accounts[12])
```

---

## 2. YENİ YAPI TASARIMI (Pseudo-Kod)

### Dizin Yapısı
```
src/
├── core/                    # Temel yapı taşları
│   ├── config.rs
│   ├── events.rs
│   ├── types.rs
│   └── error.rs
│
├── blockchain/              # Blockchain etkileşimi
│   ├── rpc_client.rs
│   ├── ws_client.rs
│   └── transaction.rs
│
├── protocol/                # Protocol abstraction
│   ├── mod.rs              # Protocol trait
│   ├── solend/
│   │   ├── mod.rs
│   │   ├── accounts.rs     # Account parsing
│   │   ├── instructions.rs # Instruction building
│   │   └── types.rs        # Solend-specific types
│   └── oracle/
│       ├── mod.rs
│       ├── pyth.rs
│       └── switchboard.rs
│
├── engine/                  # Core liquidation engine
│   ├── scanner.rs          # Account discovery & monitoring
│   ├── analyzer.rs         # Opportunity detection
│   ├── validator.rs        # Opportunity validation
│   └── executor.rs         # Transaction execution
│
├── strategy/                # Trading logic
│   ├── profit_calculator.rs
│   ├── slippage_estimator.rs
│   └── balance_manager.rs
│
├── utils/                   # Utilities
│   ├── cache.rs
│   ├── metrics.rs
│   └── helpers.rs
│
└── main.rs                  # Entry point
```

---

## 3. HER DOSYANIN İÇERİĞİ (Pseudo-Kod)

### `core/config.rs`
```rust
struct Config {
    // RPC endpoints
    rpc_http: String
    rpc_ws: String
    
    // Wallet
    wallet_path: String
    
    // Thresholds
    min_profit_usd: f64
    health_factor_threshold: f64
    max_slippage_bps: u16
    
    // Protocol
    protocol_id: String
    program_id: Pubkey
    
    // Features
    use_jupiter_api: bool
    dry_run: bool
}

impl Config {
    fn from_env() -> Result<Self>
    fn validate(&self) -> Result<()>
}
```

### `core/events.rs`
```rust
enum Event {
    // Discovery
    AccountDiscovered { pubkey, data }
    AccountUpdated { pubkey, data }
    
    // Analysis
    OpportunityFound { opportunity }
    
    // Execution
    TransactionSent { signature }
    TransactionConfirmed { signature, success }
}

struct EventBus {
    sender: Sender<Event>
    
    fn publish(event: Event)
    fn subscribe() -> Receiver<Event>
}
```

### `core/types.rs`
```rust
struct Position {
    address: Pubkey
    health_factor: f64
    collateral_usd: f64
    debt_usd: f64
    collateral_assets: Vec<Asset>
    debt_assets: Vec<Asset>
}

struct Asset {
    mint: Pubkey
    amount: u64
    amount_usd: f64
    ltv: f64
}

struct Opportunity {
    position: Position
    max_liquidatable: u64
    seizable_collateral: u64
    estimated_profit: f64
    debt_mint: Pubkey
    collateral_mint: Pubkey
}
```

### `blockchain/rpc_client.rs`
```rust
struct RpcClient {
    client: solana_client::RpcClient
    rate_limiter: RateLimiter
    
    async fn get_account(pubkey) -> Account
    async fn get_program_accounts(program_id) -> Vec<(Pubkey, Account)>
    async fn send_transaction(tx) -> Signature
    async fn get_recent_blockhash() -> Hash
    
    // Retry logic with exponential backoff
    async fn retry<F>(operation: F, max_retries: u32) -> Result<T>
}
```

### `blockchain/ws_client.rs`
```rust
struct WsClient {
    url: String
    connection: WebSocket
    subscriptions: HashMap<u64, Subscription>
    
    async fn connect() -> Result<()>
    async fn subscribe_program(program_id) -> Result<SubscriptionId>
    async fn subscribe_account(pubkey) -> Result<SubscriptionId>
    async fn listen() -> Stream<Notification>
    
    // Reconnection logic
    async fn reconnect_with_backoff()
}
```

### `blockchain/transaction.rs`
```rust
struct TransactionBuilder {
    instructions: Vec<Instruction>
    payer: Pubkey
    
    fn add_compute_budget(units: u32, price: u64) -> &mut Self
    fn add_instruction(ix: Instruction) -> &mut Self
    fn build(blockhash: Hash) -> Transaction
}

fn sign_transaction(tx: &mut Transaction, keypair: &Keypair)
async fn send_and_confirm(tx: Transaction, rpc: &RpcClient) -> Result<Signature>
```

### `protocol/mod.rs`
```rust
trait Protocol {
    fn id() -> &str
    fn program_id() -> Pubkey
    
    async fn parse_position(account) -> Option<Position>
    fn calculate_health_factor(position: &Position) -> f64
    async fn build_liquidation_ix(opportunity, liquidator) -> Instruction
    
    fn liquidation_params() -> LiquidationParams
}

struct LiquidationParams {
    bonus: f64
    close_factor: f64
    max_slippage: f64
}
```

### `protocol/solend/accounts.rs`
```rust
// Borsh deserialization structs
struct SolendObligation {
    deposits: Vec<Deposit>
    borrows: Vec<Borrow>
    deposited_value: u128
    borrowed_value: u128
    
    fn from_bytes(data: &[u8]) -> Result<Self>
    fn to_position() -> Position
}

struct SolendReserve {
    liquidity_mint: Pubkey
    collateral_mint: Pubkey
    ltv: u8
    liquidation_threshold: u8
    oracle: OracleInfo
    
    fn from_bytes(data: &[u8]) -> Result<Self>
}

// PDA derivations
fn derive_lending_market_authority(market, program) -> Pubkey
fn derive_obligation(wallet, market, program) -> Pubkey
fn get_associated_token_address(wallet, mint) -> Pubkey
```

### `protocol/solend/instructions.rs`
```rust
fn build_liquidate_obligation_ix(
    opportunity: &Opportunity,
    liquidator: &Pubkey,
    rpc: &RpcClient
) -> Result<Instruction> {
    
    // 1. Fetch obligation account
    obligation_account = fetch_obligation()
    
    // 2. Fetch reserve accounts
    repay_reserve = fetch_reserve(debt_mint)
    withdraw_reserve = fetch_reserve(collateral_mint)
    
    // 3. Derive PDAs
    lending_market_authority = derive_pda()
    source_liquidity_ata = get_ata(liquidator, debt_mint)
    destination_collateral_ata = get_ata(liquidator, collateral_mint)
    
    // 4. Build instruction data
    discriminator = sha256("global:liquidateObligation")[0..8]
    data = [discriminator, amount_le_bytes]
    
    // 5. Build accounts array (12 accounts)
    accounts = [
        source_liquidity,
        destination_collateral,
        repay_reserve,
        repay_liquidity_supply,
        withdraw_reserve,
        withdraw_collateral_supply,
        obligation,
        lending_market,
        lending_market_authority,
        liquidator (signer),
        clock_sysvar,
        token_program
    ]
    
    return Instruction { program_id, accounts, data }
}
```

### `protocol/oracle/pyth.rs`
```rust
struct PythOracle {
    async fn read_price(account: &Pubkey, rpc) -> Result<PriceData>
}

struct PriceData {
    price: f64
    confidence: f64
    expo: i32
    timestamp: i64
}

fn parse_pyth_account(data: &[u8]) -> Result<PriceData> {
    // Use pyth-sdk-solana
    feed = SolanaPriceAccount::parse(data)
    price_data = feed.get_price_no_older_than(now, max_age)
    return PriceData::from(price_data)
}
```

### `engine/scanner.rs`
```rust
struct Scanner {
    rpc: Arc<RpcClient>
    ws: Arc<WsClient>
    protocol: Arc<dyn Protocol>
    event_bus: EventBus
    cache: AccountCache
    
    // Initial discovery via RPC
    async fn discover_accounts() -> Result<usize> {
        accounts = rpc.get_program_accounts(program_id)
        
        for (pubkey, account) in accounts {
            if let Some(position) = protocol.parse_position(account) {
                cache.insert(pubkey, position)
                event_bus.publish(AccountDiscovered { pubkey, position })
            }
        }
    }
    
    // Real-time monitoring via WebSocket
    async fn start_monitoring() {
        subscription_id = ws.subscribe_program(program_id)
        
        loop {
            notification = ws.listen().await
            
            if let Some(position) = protocol.parse_position(notification.account) {
                cache.update(notification.pubkey, position)
                event_bus.publish(AccountUpdated { pubkey, position })
            }
        }
    }
    
    async fn run() {
        discover_accounts().await
        start_monitoring().await
    }
}
```

### `engine/analyzer.rs`
```rust
struct Analyzer {
    event_bus: EventBus
    protocol: Arc<dyn Protocol>
    config: Config
    
    async fn run() {
        receiver = event_bus.subscribe()
        
        loop {
            event = receiver.recv()
            
            match event {
                AccountUpdated { position } => {
                    if is_liquidatable(position) {
                        opportunity = calculate_opportunity(position)
                        event_bus.publish(OpportunityFound { opportunity })
                    }
                }
            }
        }
    }
    
    fn is_liquidatable(position: &Position) -> bool {
        position.health_factor < config.health_factor_threshold
    }
    
    async fn calculate_opportunity(position: Position) -> Option<Opportunity> {
        params = protocol.liquidation_params()
        
        // 1. Calculate liquidatable amount
        max_liquidatable = position.debt_usd * params.close_factor
        seizable_collateral = max_liquidatable * (1 + params.bonus)
        
        // 2. Select best debt/collateral pair
        (debt_mint, collateral_mint) = select_best_pair(position)
        
        // 3. Calculate profit
        gross_profit = seizable_collateral - max_liquidatable
        tx_fee = estimate_tx_fee()
        slippage = estimate_slippage(seizable_collateral)
        net_profit = gross_profit - tx_fee - slippage
        
        if net_profit < config.min_profit_usd {
            return None
        }
        
        return Some(Opportunity { ... })
    }
}
```

### `engine/validator.rs`
```rust
struct Validator {
    event_bus: EventBus
    balance_manager: BalanceManager
    config: Config
    rpc: Arc<RpcClient>
    
    async fn run() {
        receiver = event_bus.subscribe()
        
        loop {
            event = receiver.recv()
            
            match event {
                OpportunityFound { opportunity } => {
                    if validate(opportunity).await.is_ok() {
                        event_bus.publish(OpportunityApproved { opportunity })
                    }
                }
            }
        }
    }
    
    async fn validate(opp: &Opportunity) -> Result<()> {
        // 1. Check balance
        has_sufficient_balance(opp.debt_mint, opp.max_liquidatable)?
        
        // 2. Check oracle price
        check_oracle_freshness(opp.debt_mint)?
        check_oracle_freshness(opp.collateral_mint)?
        
        // 3. Verify token accounts exist
        verify_ata_exists(debt_mint)?
        verify_ata_exists(collateral_mint)?
        
        // 4. Re-check slippage
        slippage = get_realtime_slippage(opp)?
        if slippage > config.max_slippage_bps {
            return Err("Slippage too high")
        }
        
        // 5. Lock balance (prevent double-spending)
        balance_manager.reserve(opp.debt_mint, opp.max_liquidatable)?
        
        Ok(())
    }
}
```

### `engine/executor.rs`
```rust
struct Executor {
    event_bus: EventBus
    rpc: Arc<RpcClient>
    wallet: Keypair
    protocol: Arc<dyn Protocol>
    balance_manager: BalanceManager
    tx_lock: TxLock
    
    async fn run() {
        receiver = event_bus.subscribe()
        
        loop {
            event = receiver.recv()
            
            match event {
                OpportunityApproved { opportunity } => {
                    // Lock to prevent duplicate execution
                    guard = tx_lock.try_lock(opportunity.position.address)?
                    
                    execute(opportunity).await
                    
                    // Guard auto-releases lock on drop
                }
            }
        }
    }
    
    async fn execute(opp: Opportunity) -> Result<Signature> {
        // 1. Build liquidation instruction
        liq_ix = protocol.build_liquidation_ix(opp, wallet.pubkey(), rpc)
        
        // 2. Build transaction
        tx = TransactionBuilder::new()
            .add_compute_budget(200_000, 1_000)
            .add_instruction(liq_ix)
            .build(rpc.get_recent_blockhash())
        
        // 3. Sign transaction
        sign_transaction(&mut tx, &wallet)
        
        // 4. Send transaction
        signature = if config.dry_run {
            "DRY_RUN_SIGNATURE"
        } else {
            rpc.send_transaction(tx).await?
        }
        
        // 5. Release balance reservation
        balance_manager.release(opp.debt_mint, opp.max_liquidatable)
        
        event_bus.publish(TransactionSent { signature })
        
        Ok(signature)
    }
}
```

### `strategy/profit_calculator.rs`
```rust
struct ProfitCalculator {
    config: Config
    
    fn calculate_net_profit(opportunity: &Opportunity) -> f64 {
        gross = opportunity.seizable_collateral - opportunity.max_liquidatable
        
        tx_fee = calculate_tx_fee()
        slippage_cost = calculate_slippage_cost(opportunity)
        dex_fee = if needs_swap { calculate_dex_fee() } else { 0 }
        
        net = gross - tx_fee - slippage_cost - dex_fee
        
        return net
    }
    
    fn calculate_tx_fee() -> f64 {
        base_fee = 5_000 lamports
        priority_fee = compute_units * priority_fee_per_cu / 1_000_000
        total_lamports = base_fee + priority_fee
        total_usd = total_lamports * sol_price_usd / 1e9
        
        return total_usd
    }
    
    fn calculate_slippage_cost(opp: &Opportunity) -> f64 {
        size_usd = opp.seizable_collateral_usd
        
        dex_slippage = estimate_dex_slippage(size_usd)
        oracle_confidence = read_oracle_confidence(opp.collateral_mint)
        
        total_slippage_bps = dex_slippage + oracle_confidence
        final_slippage_bps = total_slippage_bps * config.slippage_multiplier
        
        cost = size_usd * (final_slippage_bps / 10_000)
        
        return cost
    }
}
```

### `strategy/slippage_estimator.rs`
```rust
struct SlippageEstimator {
    config: Config
    
    async fn estimate_dex_slippage(
        input_mint: Pubkey,
        output_mint: Pubkey,
        amount: u64
    ) -> u16 {
        
        if config.use_jupiter_api {
            // Real-time slippage from Jupiter
            quote = jupiter_api.get_quote(input_mint, output_mint, amount)
            return quote.price_impact_bps
        } else {
            // Size-based estimation
            size_usd = amount_to_usd(amount)
            
            multiplier = if size_usd < 10_000 {
                config.slippage_multiplier_small
            } else if size_usd > 100_000 {
                config.slippage_multiplier_large
            } else {
                config.slippage_multiplier_medium
            }
            
            estimated = config.max_slippage_bps * multiplier
            
            return estimated
        }
    }
    
    async fn read_oracle_confidence(mint: Pubkey) -> u16 {
        oracle_account = get_oracle_account(mint)
        price_data = read_oracle_price(oracle_account)
        
        confidence_ratio = price_data.confidence / price_data.price
        confidence_bps = (confidence_ratio * 10_000) as u16
        
        return confidence_bps
    }
}
```

### `strategy/balance_manager.rs`
```rust
struct BalanceManager {
    reserved: RwLock<HashMap<Pubkey, u64>>
    rpc: Arc<RpcClient>
    wallet: Pubkey
    
    async fn get_available_balance(mint: Pubkey) -> u64 {
        actual = get_token_balance(mint)
        reserved = self.reserved.read().get(mint).copied().unwrap_or(0)
        available = actual.saturating_sub(reserved)
        
        return available
    }
    
    async fn reserve(mint: Pubkey, amount: u64) -> Result<Guard> {
        available = get_available_balance(mint)
        
        if available < amount {
            return Err("Insufficient balance")
        }
        
        reserved.write().insert(mint, amount)
        
        return Ok(Guard { mint, amount })
    }
    
    async fn release(mint: Pubkey, amount: u64) {
        reserved.write().entry(mint).and_modify(|v| *v -= amount)
    }
}

struct Guard {
    mint: Pubkey
    amount: u64
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Auto-release on drop
        tokio::spawn(release(self.mint, self.amount))
    }
}
```

### `utils/cache.rs`
```rust
struct AccountCache {
    positions: RwLock<HashMap<Pubkey, Position>>
    
    async fn insert(pubkey: Pubkey, position: Position)
    async fn get(pubkey: &Pubkey) -> Option<Position>
    async fn update(pubkey: Pubkey, position: Position)
    async fn remove(pubkey: &Pubkey)
    
    async fn get_all_liquidatable(threshold: f64) -> Vec<Position> {
        positions.read()
            .values()
            .filter(|p| p.health_factor < threshold)
            .cloned()
            .collect()
    }
}
```

### `utils/metrics.rs`
```rust
struct Metrics {
    opportunities_found: AtomicU64
    transactions_sent: AtomicU64
    transactions_successful: AtomicU64
    total_profit_usd: AtomicF64
    
    latency: RwLock<Vec<Duration>>
    
    fn record_opportunity()
    fn record_transaction(success: bool, profit: f64)
    fn record_latency(duration: Duration)
    
    fn get_summary() -> MetricsSummary
}

struct MetricsSummary {
    opportunities: u64
    tx_sent: u64
    tx_success: u64
    success_rate: f64
    total_profit: f64
    avg_latency_ms: u64
    p95_latency_ms: u64
}
```

### `main.rs`
```rust
#[tokio::main]
async fn main() -> Result<()> {
    // 1. Load config
    config = Config::from_env()?
    config.validate()?
    
    // 2. Initialize components
    rpc = Arc::new(RpcClient::new(config.rpc_http))
    ws = Arc::new(WsClient::new(config.rpc_ws))
    wallet = load_wallet(config.wallet_path)?
    protocol = Arc::new(SolendProtocol::new(config))
    
    // 3. Create event bus
    event_bus = EventBus::new()
    
    // 4. Create managers
    balance_manager = Arc::new(BalanceManager::new(rpc, wallet.pubkey()))
    metrics = Arc::new(Metrics::new())
    cache = Arc::new(AccountCache::new())
    
    // 5. Spawn workers
    scanner = Scanner::new(rpc, ws, protocol, event_bus, cache)
    analyzer = Analyzer::new(event_bus, protocol, config)
    validator = Validator::new(event_bus, balance_manager, config, rpc)
    executor = Executor::new(event_bus, rpc, wallet, protocol, balance_manager)
    
    tokio::spawn(scanner.run())
    tokio::spawn(analyzer.run())
    tokio::spawn(validator.run())
    tokio::spawn(executor.run())
    
    // 6. Metrics logger
    tokio::spawn(async move {
        loop {
            sleep(Duration::from_secs(60))
            summary = metrics.get_summary()
            log::info!("Metrics: {:#?}", summary)
        }
    })
    
    // 7. Wait for shutdown signal
    signal::ctrl_c().await?
    
    log::info!("Shutting down gracefully...")
    Ok(())
}
```

---

## 4. DAHA TEMİZ SİSTEM İÇİN ÖNERİLER

### A) Separation of Concerns
```
Scanner    → Sadece account'ları bul ve izle
Analyzer   → Sadece opportunity'leri hesapla
Validator  → Sadece fırsat doğrula (balance, oracle, slippage)
Executor   → Sadece transaction gönder
```

### B) Event-Driven Architecture
```
Event Bus kullanarak worker'lar arasında gevşek bağlantı:
- Her worker kendi sorumluluğunu yapar
- Worker'lar birbirinden bağımsız çalışır
- Test edilebilirlik artar
```

### C) Clear Data Flow
```
RPC/WS → Scanner → [AccountUpdated] 
                ↓
              Analyzer → [OpportunityFound]
                ↓
              Validator → [OpportunityApproved]
                ↓
              Executor → [TransactionSent]
```

### D) Error Handling Strategy
```
- Her layer kendi hatalarını handle eder
- Critical errors → Reconnect/Retry
- Non-critical errors → Log & Continue
- Exponential backoff for retries
```

### E) Configuration Management
```
- Tüm parametreler config'de
- Environment variables ile override
- Runtime validation
- Sensible defaults
```

### F) Testing Strategy
```
- Unit tests: Her function bağımsız test
- Integration tests: RPC mocking ile
- End-to-end tests: Devnet'te
```

---

## 5. MEVCUT KOD ile KARŞILAŞTIRMA

### Mevcut Kodun Sorunları:
1. ❌ **Karmaşık bağımlılıklar**: Her modül birçok şeye bağımlı
2. ❌ **Unclear data flow**: Event'ler karmaşık, akış belirsiz
3. ❌ **Tightly coupled**: Protocol, math, wallet hepsi iç içe
4. ❌ **Over-engineering**: Çok fazla abstraction, gereksiz complexity
5. ❌ **Poor separation**: Balance reservation, profit calculation, transaction building hepsi karışık

### Yeni Yapının Avantajları:
1. ✅ **Clean separation**: Her worker tek sorumluluk
2. ✅ **Clear flow**: Event-driven, anlaşılır akış
3. ✅ **Modular**: Protocol, oracle, strategy ayrı
4. ✅ **Testable**: Her component bağımsız test edilebilir
5. ✅ **Maintainable**: Kod okumak ve değiştirmek kolay

---

## SONUÇ

Bu yeni yapı ile:
- **Daha az kod** (basit, anlaşılır)
- **Daha hızlı geliştirme** (modüler)
- **Daha kolay debug** (clear data flow)
- **Daha az bug** (separation of concerns)
- **Daha kolay test** (independent components)

Şimdi istersen bu pseudo-kod'u gerçek Rust koduna çevirebiliriz. Hangi modülden başlamak istersin?