# Code Flow Documentation

This document describes the complete flow of the liquidation bot, from data collection to transaction execution.

## Architecture Overview

The bot uses an **event-driven architecture** with loosely-coupled components. All communication happens through an event bus, allowing components to be independent and testable.

```
┌─────────────────────────────────────────────────────────────┐
│                    Event Bus (Broadcast)                     │
│              (tokio::broadcast::channel)                    │
└─────────────────────────────────────────────────────────────┘
         ↑                    ↑                    ↑
         │                    │                    │
    ┌────┴────┐          ┌─────┴─────┐        ┌────┴────┐
    │         │          │           │        │         │
┌───▼───┐ ┌──▼───┐  ┌───▼───┐  ┌───▼───┐ ┌──▼───┐ ┌──▼───┐
│ RPC   │ │  WS  │  │Analyzer│  │Strate-│ │Execu-│ │Logger│
│ Poller│ │Listen│  │        │  │gist   │ │tor   │ │      │
└───────┘ └──────┘  └────────┘  └───────┘ └──────┘ └──────┘
```

## Happy Path - Successful Liquidation

### 1. Data Source (`rpc_poller.rs` or `ws_listener.rs`)

**Purpose:** Fetch account data from Solana blockchain

**Flow:**
```
┌─────────────────────────────────────────────────────────────┐
│ 1. DATA SOURCE                                               │
│                                                              │
│ RPC Poller:                                                  │
│   - getProgramAccounts(program_id)                           │
│   - Returns all obligation accounts                          │
│                                                              │
│ WebSocket Listener:                                          │
│   - accountSubscribe(program_id)                            │
│   - Receives real-time account updates                       │
│                                                              │
│ For each account:                                            │
│   - parse_account_position() → AccountPosition              │
│   - Event::AccountUpdated published                         │
└─────────────────────────────────────────────────────────────┘
```

**Key Functions:**
- `rpc_poller.rs::fetch_and_publish_positions()` - Polls RPC for accounts
- `ws_listener.rs::listen_to_accounts()` - Listens to WebSocket updates
- `protocol::Protocol::parse_account_position()` - Parses account data

**References:**
- Solana RPC: https://docs.solana.com/api/http
- WebSocket API: https://docs.solana.com/api/websocket

---

### 2. Analyzer (`analyzer.rs`)

**Purpose:** Identify liquidatable positions and calculate profit

**Flow:**
```
┌─────────────────────────────────────────────────────────────┐
│ 2. ANALYZER                                                  │
│                                                              │
│ Input: Event::AccountUpdated                                 │
│                                                              │
│ Steps:                                                       │
│   1. Check health factor:                                    │
│      - HF < threshold? → Continue                            │
│      - HF >= threshold? → Skip (not liquidatable)           │
│                                                              │
│   2. Calculate liquidation opportunity:                      │
│      - calculate_liquidation_opportunity()                   │
│      - Includes:                                             │
│        * Gross profit (liquidation bonus)                   │
│        * Transaction fees                                    │
│        * Slippage (DEX + Oracle confidence)                  │
│        * Swap costs (if needed)                              │
│                                                              │
│   3. Profit check:                                           │
│      - estimated_profit >= MIN_PROFIT_USD?                   │
│      - Yes → Event::PotentiallyLiquidatable                  │
│      - No → Skip (not profitable)                           │
└─────────────────────────────────────────────────────────────┘
```

**Key Functions:**
- `analyzer.rs::run_analyzer()` - Main analyzer loop
- `math.rs::calculate_liquidation_opportunity()` - Profit calculation
- `protocol.rs::Protocol::calculate_health_factor()` - Protocol-specific health factor (uses liquidation_threshold)

**References:**
- Solend SDK: https://github.com/solendprotocol/solend-sdk
- Health Factor Formula: Uses liquidation threshold, not LTV

---

### 3. Strategist (`strategist.rs`)

**Purpose:** Apply business rules and validate opportunity

**Flow:**
```
┌─────────────────────────────────────────────────────────────┐
│ 3. STRATEGIST                                                │
│                                                              │
│ Input: Event::PotentiallyLiquidatable                        │
│                                                              │
│ Validation Steps:                                            │
│   ✓ Profit check:                                            │
│     - estimated_profit >= MIN_PROFIT_USD                     │
│                                                              │
│   ✓ Oracle staleness check:                                  │
│     - oracle_age < MAX_ORACLE_AGE_SECONDS                    │
│                                                              │
│   ✓ Slippage check:                                          │
│     - total_slippage <= MAX_SLIPPAGE_BPS                     │
│     - Formula: (DEX slippage + Oracle confidence) * multiplier│
│                                                              │
│   ✓ Balance check:                                           │
│     - Debt token balance >= required_debt_amount            │
│     - SOL balance >= MIN_RESERVE_LAMPORTS                    │
│                                                              │
│   ✓ Balance reservation (atomic):                            │
│     - try_reserve_with_check()                               │
│     - Prevents race conditions                               │
│                                                              │
│ If all checks pass:                                          │
│   - Event::ExecuteLiquidation published                     │
│                                                              │
│ If any check fails:                                          │
│   - Event discarded (logged with reason)                     │
└─────────────────────────────────────────────────────────────┘
```

**Key Functions:**
- `strategist.rs::run_strategist()` - Main strategist loop
- `balance_reservation.rs::try_reserve_with_check()` - Atomic balance reservation
- `wallet.rs::WalletBalanceChecker` - Balance validation

**References:**
- Balance Reservation: Prevents race conditions in parallel processing
- Oracle Staleness: https://docs.pyth.network/price-feeds

---

### 4. Executor (`executor.rs`)

**Purpose:** Execute liquidation transaction

**Flow:**
```
┌─────────────────────────────────────────────────────────────┐
│ 4. EXECUTOR                                                  │
│                                                              │
│ Input: Event::ExecuteLiquidation                             │
│                                                              │
│ Steps:                                                       │
│   1. Transaction lock:                                       │
│      - tx_lock.try_lock(account_address)                    │
│      - Prevents duplicate transactions                      │
│      - If locked → Skip (already processing)                │
│                                                              │
│   2. Build liquidation instruction:                          │
│      - protocol.build_liquidation_instruction()             │
│      - Resolves all required accounts                        │
│      - Creates instruction with correct account order        │
│                                                              │
│   3. Prepare transaction:                                    │
│      - Add compute budget instruction                        │
│      - Add priority fee instruction                          │
│      - Sign transaction                                      │
│                                                              │
│   4. Send transaction:                                       │
│      - rpc_client.send_transaction()                        │
│      - Retry on failure (max 3 attempts)                    │
│      - Exponential backoff between retries                   │
│                                                              │
│   5. Publish result:                                         │
│      - Event::TxResult published                            │
│      - Contains: success, signature, error                   │
│                                                              │
│   6. Unlock account:                                         │
│      - tx_lock.unlock() (via Drop guard)                    │
└─────────────────────────────────────────────────────────────┘
```

**Key Functions:**
- `executor.rs::run_executor()` - Main executor loop
- `executor.rs::execute_liquidation()` - Transaction execution
- `tx_lock.rs::TxLock` - Duplicate prevention
- `solana_client.rs::send_transaction()` - RPC call

**References:**
- Solend Program: https://github.com/solendprotocol/solana-program-library
- Instruction Format: See `protocols/solend.rs::build_liquidation_instruction()`

---

### 5. Logger (`logger.rs`)

**Purpose:** Log events and update metrics

**Flow:**
```
┌─────────────────────────────────────────────────────────────┐
│ 5. LOGGER                                                    │
│                                                              │
│ Input: All events                                            │
│                                                              │
│ Actions:                                                     │
│   - Log all events with appropriate log level                │
│   - Update metrics (opportunities found, executed, etc.)     │
│   - Notify health manager of errors                          │
│   - Track performance metrics                                │
└─────────────────────────────────────────────────────────────┘
```

**Key Functions:**
- `logger.rs::run_logger()` - Main logger loop
- `performance.rs::PerformanceMetrics` - Metrics tracking
- `health.rs::HealthManager` - Health monitoring

---

## Error Paths

### RPC Rate Limit (429)

```
RPC Rate Limit (429)
    ↓
rpc_poller.rs::fetch_with_retry()
    ↓
Exponential backoff (2^attempt seconds) + jitter
    ↓
Max 5 retries → Error logged
    ↓
Continue polling (next interval)
```

**Reference:** `solana_client.rs::fetch_with_retry()`

---

### Health Factor > Threshold

```
Health Factor >= 1.0
    ↓
analyzer.rs::run_analyzer()
    ↓
Skip (not liquidatable)
    ↓
No event published
```

---

### Profit < Minimum

```
estimated_profit < MIN_PROFIT_USD
    ↓
analyzer.rs::run_analyzer()
    ↓
Skip (not profitable)
    ↓
No event published
```

---

### Insufficient Balance

```
Debt balance < required_debt_amount
    OR
SOL balance < MIN_RESERVE_LAMPORTS
    ↓
strategist.rs::run_strategist()
    ↓
Reject with reason: "insufficient capital"
    ↓
Event discarded
```

---

### Balance Reserved by Another Transaction

```
Balance already reserved
    ↓
balance_reservation.rs::try_reserve_with_check()
    ↓
Returns None (reservation failed)
    ↓
strategist.rs::run_strategist()
    ↓
Reject with reason: "race condition prevented"
    ↓
Event discarded
```

**Reference:** `balance_reservation.rs::try_reserve_with_check()`

---

### Account Already Locked

```
Account already locked (duplicate transaction)
    ↓
tx_lock.rs::try_lock()
    ↓
Returns false (lock failed)
    ↓
executor.rs::run_executor()
    ↓
Skip (duplicate prevention)
    ↓
No transaction sent
```

**Reference:** `tx_lock.rs::TxLock`

---

### Transaction Failed

```
Transaction send failed
    ↓
executor.rs::execute_liquidation()
    ↓
Retry (max 3 attempts)
    ↓
Exponential backoff between retries
    ↓
All retries failed
    ↓
Event::TxResult{success: false, error: ...}
    ↓
Published to event bus
```

**Reference:** `executor.rs::execute_liquidation()`

---

## Event Types

### Event::AccountUpdated
- **Source:** RPC Poller / WebSocket Listener
- **Data:** `AccountPosition`
- **Consumers:** Analyzer

### Event::PotentiallyLiquidatable
- **Source:** Analyzer
- **Data:** `LiquidationOpportunity`
- **Consumers:** Strategist

### Event::ExecuteLiquidation
- **Source:** Strategist
- **Data:** `LiquidationOpportunity`
- **Consumers:** Executor

### Event::TxResult
- **Source:** Executor
- **Data:** `{opportunity, success, signature, error}`
- **Consumers:** Logger

---

## Key Design Decisions

### 1. Event-Driven Architecture
- **Why:** Loose coupling, easy testing, scalable
- **Implementation:** `tokio::broadcast::channel`

### 2. Atomic Balance Reservation
- **Why:** Prevent race conditions in parallel processing
- **Implementation:** `balance_reservation.rs::try_reserve_with_check()`

### 3. Transaction Locking
- **Why:** Prevent duplicate transactions for same account
- **Implementation:** `tx_lock.rs::TxLock`

### 4. Retry with Exponential Backoff
- **Why:** Handle transient RPC errors gracefully
- **Implementation:** `solana_client.rs::fetch_with_retry()`

### 5. Protocol Abstraction
- **Why:** Support multiple protocols (currently Solend)
- **Implementation:** `protocol.rs::Protocol` trait

---

## References

- Solana Documentation: https://docs.solana.com/
- Solend Protocol: https://docs.solend.fi/
- Pyth Network: https://docs.pyth.network/
- Event-Driven Architecture: https://martinfowler.com/articles/201701-event-driven.html

