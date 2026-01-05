# Анализ движка perp-futures: Проблемы и Рекомендации

> **Дата анализа:** Декабрь 2024  
> **Версия движка:** commit 94953de  
> **Репозиторий:** https://github.com/LouiseMedova/perp-futures

---

## 📋 Содержание

1. [Критические проблемы](#1-критические-проблемы)
2. [Нереализованные компоненты](#2-нереализованные-компоненты)
3. [Необходимые API](#3-необходимые-api)
4. [Расширение структур данных](#4-расширение-структур-данных)
5. [Приоритеты реализации](#5-приоритеты-реализации)

---

## 1. Критические проблемы

### 1.1 ❌ Невозможно закрыть убыточную позицию

**Файл:** `executor.rs` (строка 456)

**Код проблемы:**

```rust
if loss > pos.collateral_amount {
    if is_liq && is_full_close {
        // OK только для ликвидации
    } else {
        return Err("insufficient_collateral_for_negative_pnl".into());
    }
}
```

**Причина:** При большом мгновенном убытке (slippage + fees) позиция может уйти в минус больше, чем collateral.

**Рекомендация:**

```rust
// Вариант 1: Разрешить закрытие с нулевым payout
pub fn force_close_position(pos: &mut Position) -> Result<(), String>;

// Вариант 2: При закрытии не блокировать, а просто обнулять payout
if loss > pos.collateral_amount && !is_liq {
    // Закрываем с нулевым payout вместо ошибки
    pos.collateral_amount = 0;
    return Ok(DecreaseResult { output_tokens: 0, ... });
}
```

---

### 1.2 ❌ PriceImpactLargerThanOrderSize при закрытии

**Файл:** `services/pricing.rs` (строки 159-164)

**Код проблемы:**

```rust
if size_delta_tokens < 0 {
    return Err(PricingError::PriceImpactLargerThanOrderSize {
        price_impact_usd,
        size_delta_usd,
    });
}
```

**Причина:** При большом дисбалансе OI (много longs vs shorts) price_impact становится огромным.

**Пример из симуляции:**

```
error=pricing_error:PriceImpactLargerThanOrderSize {
  price_impact_usd: -872251919637339,  // -$872M price impact!
  size_delta_usd: 17809618650          // $17K position
}
```

**Рекомендация:**

- Добавить cap на максимальный price impact (например, 50% от size)
- Или разрешить закрытие с ограниченным price impact

---

### 1.3 ❌ "position_empty_or_corrupted" после неудачного закрытия

**Причина:** После ошибки `insufficient_collateral_for_negative_pnl` позиция остаётся в состоянии с `size_usd > 0`, но при следующей попытке закрыть возвращает эту ошибку.

**Рекомендация:**

- Добавить функцию очистки "corrupted" позиций
- Или автоматически помечать как "requires_liquidation"

---

## 2. Нереализованные компоненты

### 2.1 ❌ MarginService полностью не реализован

**Файл:** `services/margin.rs`

```rust
pub trait MarginService {
    // fn pre_check_increase(...);        // ЗАКОММЕНТИРОВАНО
    // fn post_check_increase(...);       // ЗАКОММЕНТИРОВАНО
    // fn pre_check_decrease(...);        // ЗАКОММЕНТИРОВАНО
    // fn post_check_decrease(...);       // ЗАКОММЕНТИРОВАНО
    // fn can_liquidate(...) -> bool;     // ЗАКОММЕНТИРОВАНО
}

pub struct BasicMarginService;
impl MarginService for BasicMarginService {}  // ПУСТАЯ РЕАЛИЗАЦИЯ!
```

**Что нужно реализовать:**

```rust
impl MarginService for BasicMarginService {
    fn can_liquidate(
        &self,
        pos: &Position,
        prices: &OraclePrices,
        risk: &RiskCfg,
    ) -> bool {
        let pnl = total_position_pnl_usd(pos, prices).unwrap_or(0);
        let remaining_margin = pos.collateral_amount + pnl;
        let maintenance_margin = pos.size_usd * risk.min_collateral_factor_fp / risk.factor_scale;
        remaining_margin < maintenance_margin
    }
}
```

---

### 2.2 ❌ Нет функции get_liquidation_price()

**Необходимо добавить:**

```rust
/// Рассчитать цену ликвидации для позиции
pub fn get_liquidation_price(
    pos: &Position,
    risk: &RiskCfg,
) -> Result<Usd, String> {
    if pos.size_tokens == 0 {
        return Err("invalid_position".into());
    }

    let maintenance_margin = pos.size_usd * risk.min_collateral_factor_fp / risk.factor_scale;

    // Формула: при какой цене remaining_margin = maintenance_margin
    // remaining_margin = collateral + pnl
    // pnl = size_tokens * price - size_usd (Long)
    // pnl = size_usd - size_tokens * price (Short)

    match pos.key.side {
        Side::Long => {
            // collateral + (tokens * liq_price - size_usd) = maintenance
            // liq_price = (maintenance - collateral + size_usd) / tokens
            let numerator = maintenance_margin - pos.collateral_amount + pos.size_usd;
            Ok(numerator / pos.size_tokens)
        }
        Side::Short => {
            // collateral + (size_usd - tokens * liq_price) = maintenance
            // liq_price = (size_usd + collateral - maintenance) / tokens
            let numerator = pos.size_usd + pos.collateral_amount - maintenance_margin;
            Ok(numerator / pos.size_tokens)
        }
    }
}
```

---

## 3. Необходимые API

### 3.1 Position Info API

```rust
/// Полная информация о позиции с расчётами
pub struct PositionInfo {
    // Базовые данные из Position
    pub size_usd: Usd,
    pub size_tokens: TokenAmount,
    pub collateral: TokenAmount,
    pub opened_at: Timestamp,

    // Рассчитанные значения
    pub entry_price: Usd,           // weighted average
    pub current_price: Usd,
    pub leverage_actual: f64,       // size_usd / collateral

    // PnL breakdown
    pub unrealized_pnl: Usd,        // от движения цены
    pub funding_fee_accrued: Usd,   // накопленная funding fee
    pub borrowing_fee_accrued: Usd, // накопленная borrowing fee
    pub total_pnl: Usd,             // pnl - fees

    // Ликвидация
    pub liquidation_price: Usd,
    pub margin_ratio: f64,          // remaining_margin / size_usd
    pub is_liquidatable: bool,
}

pub fn get_position_info(
    pos: &Position,
    market: &MarketState,
    prices: &OraclePrices,
    now: Timestamp,
) -> Result<PositionInfo, String>;
```

### 3.2 Market Rates API

```rust
/// Текущие ставки рынка
pub struct MarketRates {
    pub funding_rate_per_hour: f64,
    pub borrowing_rate_per_hour: f64,
    pub utilization: f64,           // total_oi / liquidity
    pub oi_imbalance: f64,          // (long - short) / (long + short)
}

pub fn get_market_rates(market: &MarketState) -> MarketRates;
```

### 3.3 Price Impact Estimation

```rust
/// Оценка price impact до исполнения
pub struct PriceImpactEstimate {
    pub execution_price: Usd,
    pub price_impact_usd: Usd,
    pub price_impact_pct: f64,
    pub balance_improved: bool,
}

pub fn estimate_price_impact(
    market_id: MarketId,
    side: Side,
    size_delta_usd: Usd,
    state: &State,
    prices: &OraclePrices,
) -> Result<PriceImpactEstimate, String>;
```

---

## 4. Расширение структур данных

### 4.1 Position — новые поля

```rust
pub struct Position {
    // ===== Существующие поля =====
    pub key: PositionKey,
    pub size_usd: Usd,
    pub size_tokens: TokenAmount,
    pub collateral_amount: TokenAmount,
    pub pending_impact_tokens: TokenAmount,
    pub funding_index: i128,
    pub borrowing_index: i128,
    pub opened_at: Timestamp,
    pub last_updated_at: Timestamp,

    // ===== НОВЫЕ ПОЛЯ =====

    /// Weighted average entry price
    /// Более точный чем size_usd/size_tokens
    pub entry_price: Usd,

    /// Total realized PnL from partial closes
    pub realized_pnl: Usd,

    /// Accumulated funding fees (positive = paid, negative = received)
    pub accumulated_funding_fee: Usd,

    /// Accumulated borrowing fees
    pub accumulated_borrowing_fee: Usd,

    /// Number of increase operations
    pub increase_count: u32,

    /// Number of decrease operations (partial closes)
    pub decrease_count: u32,
}
```

### 4.2 ExecutionResult — детали исполнения

```rust
/// Детали исполнения ордера (для логирования/анализа)
pub struct ExecutionResult {
    pub order_id: OrderId,
    pub execution_price: Usd,
    pub size_delta_usd: Usd,
    pub size_delta_tokens: TokenAmount,
    pub collateral_delta: TokenAmount,
    pub price_impact_usd: Usd,
    pub position_fee_usd: Usd,
    pub funding_fee_usd: Usd,
    pub borrowing_fee_usd: Usd,
    pub realized_pnl: Usd,  // для decrease
}
```

---

## 5. Приоритеты реализации

### 🔴 Критический (блокирует работу)

| #   | Задача                               | Файл          | Сложность |
| --- | ------------------------------------ | ------------- | --------- |
| 1   | Разрешить закрытие убыточных позиций | `executor.rs` | Низкая    |
| 2   | Реализовать `can_liquidate()`        | `margin.rs`   | Средняя   |
| 3   | Добавить `get_liquidation_price()`   | новый         | Средняя   |

### 🟡 Важный (для полноценного тестирования)

| #   | Задача                            | Файл                | Сложность |
| --- | --------------------------------- | ------------------- | --------- |
| 4   | Добавить `entry_price` в Position | `position_store.rs` | Низкая    |
| 5   | Реализовать `get_position_info()` | новый               | Средняя   |
| 6   | Добавить cap на price impact      | `pricing.rs`        | Низкая    |
| 7   | Реализовать `get_market_rates()`  | новый               | Низкая    |

### 🟢 Желательный (для продвинутого анализа)

| #   | Задача                                | Файл                | Сложность |
| --- | ------------------------------------- | ------------------- | --------- |
| 9   | Добавить accumulated fees в Position  | `position_store.rs` | Низкая    |
| 10  | Реализовать `estimate_price_impact()` | новый               | Низкая    |
| 11  | Добавить ExecutionResult              | новый               | Средняя   |

---

## 6. Связь проблем

```
Мгновенный убыток (из-за slippage/fees)
        │
        ├──► insufficient_collateral_for_negative_pnl (1.1)
        │           │
        │           ▼
        │    Позиция не закрывается
        │           │
        │           ▼
        │    position_empty_or_corrupted (1.3)
        │
        └──► Неверный is_liquidatable (false positives)
```

**Вывод:** Решение проблемы блокировки закрытия убыточных позиций (1.1) критически важно.

---

## 7. Контакты

При возникновении вопросов по этому анализу — обращаться к команде perp-lab.

**Репозиторий симулятора:** perp-lab  
**Репозиторий движка:** https://github.com/LouiseMedova/perp-futures
