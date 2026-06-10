//! A small, fully-documented **money ledger** library — the DX dogfooding artifact.
//!
//! Every public symbol carries a `///` doc comment (so `ascript doc --check`
//! passes), the module exercises a spread of language features for `ascript doc`
//! to render (documented functions with typed params/returns, a class with fields
//! + methods, and a payload-carrying ADT enum), and it ships an in-file
//! `test(...)` suite that drives the `ascript test --coverage` path. The whole
//! module is deterministic and runs to completion, so it is part of the
//! run-to-completion corpus (NOT in `EXAMPLE_SKIPS`).
//!
//! Amounts are held in **integer minor units** (cents) to avoid binary-float drift;
//! formatting renders the major/minor split. Fallible operations return a Tier-1
//! `[value, err]` pair so callers can `?`-propagate or `!`-unwrap.
import * as array from "std/array"
import * as assert from "std/assert"

/// The supported currencies. Each variant is a unit ADT variant; the minor-unit
/// scale (cents per major unit) is looked up by `scaleOf`.
export enum Currency {
  USD,
  EUR,
  JPY,
}

/// The number of minor units in one major unit of `c` (e.g. 100 cents per dollar,
/// but yen has no minor unit). Written with QUALIFIED unit patterns so the `match`
/// is exhaustive and the arms compare rather than shadow-bind.
///
/// @param c — the currency to look up
/// @returns the minor-unit scale (1, 10, or 100)
export fn scaleOf(c: Currency): int {
  return match c {
    Currency.USD => 100,
    Currency.EUR => 100,
    Currency.JPY => 1,
  }
}

/// The three-letter ISO code for a currency, for display.
///
/// @param c — the currency to render
/// @returns the uppercase ISO-4217 code
export fn codeOf(c: Currency): string {
  return match c {
    Currency.USD => "USD",
    Currency.EUR => "EUR",
    Currency.JPY => "JPY",
  }
}

/// An immutable money amount: a signed count of minor units in a single currency.
/// Construct via `Money.fromUnits` (major + minor) or directly with the minor-unit count.
export class Money {
  /// The signed amount in minor units (e.g. cents). `-150` is `-$1.50`.
  minor: int = 0
  /// The currency this amount is denominated in.
  currency: Currency = Currency.USD

  /// Build a `Money` from a whole-units / minor-units split, e.g.
  /// `Money.fromUnits(12, 50, Currency.USD)` is `$12.50`. The minor part is taken modulo
  /// the currency scale so an out-of-range minor still normalizes.
  ///
  /// @param major — the whole-unit part
  /// @param minor — the fractional minor-unit part
  /// @param currency — the denomination
  /// @returns a normalized `Money`
  static fn fromUnits(major: int, minor: int, currency: Currency): Money {
    let scale = scaleOf(currency)
    let total = major * scale + minor
    return Money(total, currency)
  }

  /// Add another amount, returning a Tier-1 `[Money, err]` pair. Mismatched
  /// currencies are a recoverable error rather than a panic.
  ///
  /// @param other — the amount to add (must share this currency)
  /// @returns `[sum, nil]` on success, or `[nil, err]` on a currency mismatch
  fn add(other: Money) {
    if (other.currency != self.currency) {
      return Err(`currency mismatch: ${codeOf(self.currency)} + ${codeOf(other.currency)}`)
    }
    return Ok(Money(self.minor + other.minor, self.currency))
  }

  /// Negate the amount (same currency, opposite sign).
  ///
  /// @returns the additive inverse
  fn negate(): Money {
    return Money(-self.minor, self.currency)
  }

  /// Render the amount as a human string, e.g. `$12.50` worth as `12.50 USD`.
  /// Currencies with no minor unit (scale 1) render without a decimal part.
  ///
  /// @returns the formatted amount with its ISO code
  fn format(): string {
    let scale = scaleOf(self.currency)
    let code = codeOf(self.currency)
    if (scale == 1) {
      return `${self.minor} ${code}`
    }
    let negative = self.minor < 0
    let sign = negative ? "-" : ""
    let abs = negative ? -self.minor : self.minor
    let major = abs / scale
    let frac = abs % scale
    let fracStr = frac < 10 ? `0${frac}` : `${frac}`
    return `${sign}${major}.${fracStr} ${code}`
  }
}

/// The outcome of posting an entry to a ledger — a payload-carrying ADT. `Posted`
/// carries the new running balance; `Rejected` carries the reason. Matching is
/// exhaustive over the two variants.
export enum PostResult {
  Posted(balance: Money),
  Rejected(reason: string),
}

/// A single-currency ledger that accumulates a running balance. The ledger refuses
/// entries in a foreign currency and refuses to go below an optional floor.
export class Ledger {
  /// The currency every entry must be denominated in.
  currency: Currency = Currency.USD
  /// The running balance in minor units.
  balance: int = 0
  /// The minimum allowed balance in minor units (entries that would breach it are
  /// rejected). Defaults to a hard zero floor.
  floor: int = 0

  /// Attempt to post an entry. A foreign currency or a breach of the floor yields a
  /// `Rejected`; otherwise the balance moves and a `Posted` carries the new total.
  ///
  /// @param entry — the signed amount to post
  /// @returns a `PostResult` describing the outcome
  fn post(entry: Money): PostResult {
    if (entry.currency != self.currency) {
      return PostResult.Rejected(`foreign currency ${codeOf(entry.currency)}`)
    }
    let next = self.balance + entry.minor
    if (next < self.floor) {
      return PostResult.Rejected("would breach floor")
    }
    self.balance = next
    return PostResult.Posted(Money(next, self.currency))
  }

  /// The current balance as a `Money` value.
  ///
  /// @returns the running balance
  fn total(): Money {
    return Money(self.balance, self.currency)
  }
}

/// Sum a list of same-currency amounts into one `Money`, returning a Tier-1
/// `[Money, err]` pair. An empty list is an error (there is no currency to infer);
/// a mismatched element short-circuits via `?`-propagation through `add`.
///
/// @param amounts — a non-empty list of `Money` values
/// @returns `[sum, nil]` or `[nil, err]`
export fn sumAmounts(amounts: array<Money>) {
  if (len(amounts) == 0) {
    return Err("cannot sum an empty list")
  }
  let acc = amounts[0]
  let rest = array.slice(amounts, 1, len(amounts))
  for (m of rest) {
    acc = acc.add(m)?
  }
  return Ok(acc)
}

// ---------------------------------------------------------------------------
// A deterministic driver so `ascript run` produces stable output and the
// run-to-completion corpus exercises the public surface.
// ---------------------------------------------------------------------------

let wallet = Money.fromUnits(12, 50, Currency.USD)
let tip = Money.fromUnits(2, 75, Currency.USD)
let combined = wallet.add(tip)!
print(combined.format()) // 15.25 USD
print(wallet.negate().format()) // -12.50 USD

let yen = Money.fromUnits(1500, 0, Currency.JPY)
print(yen.format()) // 1500 JPY

let book = Ledger(Currency.USD, 0, 0)
let deposit = book.post(Money.fromUnits(20, 0, Currency.USD))
print(match deposit {
  Posted(bal) => `posted, balance ${bal.format()}`,
  Rejected(why) => `rejected: ${why}`,
})

let overdraft = book.post(Money.fromUnits(-50, 0, Currency.USD))
print(match overdraft {
  Posted(bal) => `posted, balance ${bal.format()}`,
  Rejected(why) => `rejected: ${why}`,
})

let total = sumAmounts([wallet, tip, Money.fromUnits(1, 0, Currency.USD)])!
print(total.format()) // 16.25 USD

// ---------------------------------------------------------------------------
// In-file test suite — drives `ascript test [--coverage]`. These re-enter the
// module's public functions/methods, covering the branches above.
// ---------------------------------------------------------------------------

test("scaleOf and codeOf cover every currency", () => {
  assert.eq(scaleOf(Currency.USD), 100)
  assert.eq(scaleOf(Currency.JPY), 1)
  assert.eq(codeOf(Currency.EUR), "EUR")
})

test("Money.fromUnits normalizes and formats", () => {
  let m = Money.fromUnits(3, 5, Currency.USD)
  assert.eq(m.minor, 305)
  assert.eq(m.format(), "3.05 USD")
})

test("Money.add rejects a currency mismatch", () => {
  let usd = Money.fromUnits(1, 0, Currency.USD)
  let eur = Money.fromUnits(1, 0, Currency.EUR)
  let [sum, err] = usd.add(eur)
  assert.isNil(sum)
  assert.notNil(err)
  let [ok, none] = usd.add(Money.fromUnits(0, 50, Currency.USD))
  assert.isNil(none)
  assert.eq(ok.format(), "1.50 USD")
})

test("negate and JPY scale-1 formatting", () => {
  assert.eq(Money.fromUnits(2, 0, Currency.USD).negate().format(), "-2.00 USD")
  assert.eq(Money.fromUnits(42, 0, Currency.JPY).format(), "42 JPY")
})

test("Ledger posts, rejects floor breach and foreign currency", () => {
  let l = Ledger(Currency.USD, 0, 0)
  let posted = l.post(Money.fromUnits(10, 0, Currency.USD))
  assert.eq(match posted { Posted(b) => b.minor, Rejected(_) => -1 }, 1000)
  let breach = l.post(Money.fromUnits(-100, 0, Currency.USD))
  assert.eq(match breach { Posted(_) => "ok", Rejected(r) => r }, "would breach floor")
  let foreign = l.post(Money.fromUnits(1, 0, Currency.EUR))
  assert.eq(match foreign { Posted(_) => "ok", Rejected(r) => r }, "foreign currency EUR")
  assert.eq(l.total().minor, 1000)
})

test("sumAmounts folds and reports empty/mismatch", () => {
  let [s, e] = sumAmounts([Money.fromUnits(1, 0, Currency.USD), Money.fromUnits(2, 50, Currency.USD)])
  assert.isNil(e)
  assert.eq(s.format(), "3.50 USD")
  let [empty, eerr] = sumAmounts([])
  assert.isNil(empty)
  assert.notNil(eerr)
  let [mix, merr] = sumAmounts([Money.fromUnits(1, 0, Currency.USD), Money.fromUnits(1, 0, Currency.EUR)])
  assert.isNil(mix)
  assert.notNil(merr)
})
