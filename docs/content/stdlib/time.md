:::eyebrow Standard library

# Time & locale

Three modules cover the passage of time and locale-aware presentation. Throughout AScript, **durations are plain numbers of milliseconds** — there is no dedicated duration type. A "2 second" delay is just the number `2000`, and the `time.seconds`/`time.minutes`/`time.hours` helpers exist only to convert larger units into that millisecond count.

Feature gates:

- `std/time` is **always available** — there is no feature gate.
- `std/date` requires the `datetime` feature (**on by default**).
- `std/intl` requires the `intl` feature (**on by default**).

## std/time

Wall-clock time, a monotonic clock for measuring elapsed time, asynchronous sleep, and duration-unit helpers. All values are numbers of milliseconds.

### now

Current wall-clock time as Unix epoch milliseconds (UTC).

- **Returns:** number — milliseconds since the Unix epoch (1970-01-01T00:00:00Z).

Use `time.now` for timestamps and human-facing dates. Because it tracks the wall clock, it can jump backward or forward when the system clock is adjusted (NTP, manual changes, DST), so it is **not** suitable for measuring how long something took.

```ascript
let ts = time.now()
print("epoch ms: " + ts)
```

### monotonic

A monotonic clock reading, measured from a fixed process-start instant.

- **Returns:** number — milliseconds elapsed since the program started.

`time.monotonic` never goes backward and is unaffected by system-clock changes, making it the correct choice for measuring elapsed time. Only differences between two readings are meaningful; the absolute value has no calendar meaning.

```ascript
let start = time.monotonic()
await time.sleep(50)
let elapsed = time.monotonic() - start
print("took " + elapsed + " ms")
```

### sleep

Asynchronously suspends the current task for the given duration.

- **Parameters:** `ms` (number) — milliseconds to sleep; must be non-negative.
- **Returns:** nil.

`time.sleep` is **async** and must be `await`ed. A negative duration is rejected. A fractional duration is truncated toward zero (`sleep(20.7)` sleeps 20 whole milliseconds).

```ascript
await time.sleep(time.seconds(1))   // sleep one second
```

### millis

Identity helper that returns its argument unchanged, for readability.

- **Parameters:** `n` (number) — a count of milliseconds.
- **Returns:** number — the same value, in milliseconds.

```ascript
let d = time.millis(250)   // 250
```

### seconds

Converts seconds to milliseconds.

- **Parameters:** `n` (number) — a count of seconds.
- **Returns:** number — `n * 1000`.

```ascript
let d = time.seconds(2)   // 2000
```

### minutes

Converts minutes to milliseconds.

- **Parameters:** `n` (number) — a count of minutes.
- **Returns:** number — `n * 60000`.

```ascript
let d = time.minutes(1)   // 60000
```

### hours

Converts hours to milliseconds.

- **Parameters:** `n` (number) — a count of hours.
- **Returns:** number — `n * 3600000`.

```ascript
let d = time.hours(1)   // 3600000
```

## std/date

Civil dates over the UTC epoch, backed by chrono. The central data type is the **instant** — a plain object snapshot of a moment in time. Its `epochMs` field is canonical: all arithmetic operates on it, and the other fields are derived (UTC) views.

An instant object has these fields:

| Field         | Type   | Description                                            |
| ------------- | ------ | ------------------------------------------------------ |
| `epochMs`     | number | Milliseconds since the Unix epoch (UTC). Canonical.    |
| `year`        | number | Calendar year (UTC).                                   |
| `month`       | number | Month, 1–12 (UTC).                                      |
| `day`         | number | Day of month, 1–31 (UTC).                              |
| `hour`        | number | Hour, 0–23 (UTC).                                       |
| `minute`      | number | Minute, 0–59 (UTC).                                     |
| `second`      | number | Second, 0–59 (UTC).                                     |
| `millisecond` | number | Sub-second milliseconds, 0–999 (UTC).                  |
| `weekday`     | number | Day of week, 0 = Sunday … 6 = Saturday.                |
| `iso`         | string | RFC 3339 / ISO 8601 representation.                    |

> [!NOTE]
> Instant component fields are always computed in **UTC**. Timezones in `std/date` are handled purely as fixed offsets in minutes (see `format`) — named IANA zones such as `America/New_York` are **not** supported.

### now

Current moment as an instant.

- **Returns:** instant — a snapshot of the current UTC time.

```ascript
let n = date.now()
print(n.year)
```

### fromEpochMs

Builds an instant from a Unix epoch-millis value.

- **Parameters:** `ms` (number) — milliseconds since the Unix epoch (UTC).
- **Returns:** instant.

An epoch outside chrono's representable range is rejected (Tier-2 panic).

```ascript
let inst = date.fromEpochMs(1609459200000)   // 2021-01-01T00:00:00Z
print(inst.iso)
```

### parse

Parses a date string into an instant, returning a `[instant, err]` result pair.

- **Parameters:**
  - `text` (string) — the date string to parse.
  - `fmt` (string, optional) — a chrono `strftime` format. When omitted (or nil), the default is RFC 3339 / ISO 8601, falling back to `%Y-%m-%dT%H:%M:%S`.
- **Returns:** a `[instant, err]` pair. On success the first element is the instant and the second is `nil`; on failure the first element is `nil` and the second is an error.

> [!TIER1]
> `date.parse` is a **Tier-1** operation: a malformed input does not panic. It returns the pair `[nil, err]` with a descriptive error, so callers must check the error element.

```ascript
let [inst, err] = date.parse("2021-06-15T12:30:00Z")
if err != nil {
    print("bad date: " + err.message)
} else {
    print(inst.year)   // 2021
}

// custom format
let [d, e] = date.parse("15/06/2021 12:30:00", "%d/%m/%Y %H:%M:%S")
```

### format

Renders an instant to a string using a chrono `strftime` pattern, optionally shifted by a fixed timezone offset.

- **Parameters:**
  - `instant` (instant) — the moment to format.
  - `fmt` (string) — a chrono `strftime` format string.
  - `tzOffsetMinutes` (number, optional) — minutes to shift the displayed wall-clock by. Defaults to `0` (UTC).
- **Returns:** string.

The offset only changes the displayed wall-clock time; it does not change the underlying instant.

```ascript
let inst = date.fromEpochMs(1609459200000)   // 2021-01-01T00:00:00Z
date.format(inst, "%Y-%m-%d")                // "2021-01-01"
date.format(inst, "%Y-%m-%d %H:%M", 120)     // "2021-01-01 02:00"  (UTC+2)
date.format(inst, "%Y-%m-%d %H:%M", -300)    // "2020-12-31 19:00"  (UTC-5)
```

### addDays

Returns a new instant offset by a whole number of days.

- **Parameters:** `instant` (instant); `n` (number) — days to add (may be negative).
- **Returns:** instant.

```ascript
let next = date.addDays(date.now(), 1)
```

### addHours

Returns a new instant offset by a whole number of hours.

- **Parameters:** `instant` (instant); `n` (number) — hours to add (may be negative).
- **Returns:** instant.

```ascript
let later = date.addHours(date.now(), 3)
```

### addMinutes

Returns a new instant offset by a whole number of minutes.

- **Parameters:** `instant` (instant); `n` (number) — minutes to add (may be negative).
- **Returns:** instant.

```ascript
let later = date.addMinutes(date.now(), 90)
```

### addSeconds

Returns a new instant offset by a whole number of seconds.

- **Parameters:** `instant` (instant); `n` (number) — seconds to add (may be negative).
- **Returns:** instant.

```ascript
let later = date.addSeconds(date.now(), 30)
```

### addMonths

Returns a new instant offset by a whole number of calendar months. The day is clamped to the target month's length.

- **Parameters:** `instant` (instant); `n` (number) — months to add (may be negative).
- **Returns:** instant.

```ascript
let [jan31, _] = date.parse("2021-01-31T00:00:00Z")
let feb = date.addMonths(jan31, 1)
print(feb.month)   // 2
print(feb.day)     // 28  (clamped; 2021 is not a leap year)
```

### addYears

Returns a new instant offset by a whole number of years (implemented as 12-month steps, so the day is likewise clamped — e.g. Feb 29 → Feb 28 in a non-leap year).

- **Parameters:** `instant` (instant); `n` (number) — years to add (may be negative).
- **Returns:** instant.

```ascript
let nextYear = date.addYears(date.now(), 1)
```

### diffMs

Difference between two instants, in milliseconds.

- **Parameters:** `a` (instant); `b` (instant).
- **Returns:** number — `a.epochMs - b.epochMs`.

```ascript
let base = date.fromEpochMs(1609459200000)
let plus1 = date.addDays(base, 1)
date.diffMs(plus1, base)   // 86400000  (one day in ms)
```

## std/intl

Locale-aware number, currency and date formatting, case folding, and collation. Locales are BCP-47 strings such as `"en-US"`, `"de-DE"`, or `"tr"`.

> [!NOTE]
> `std/intl` is a **pragmatic subset of ICU**. `formatNumber`, `caseUpper`, `caseLower`, and `compare` use real ICU algorithms. `formatCurrency` and `formatDate` are **pragmatic fallbacks**: currency formatting always uses a small symbol table placed as a prefix (not full CLDR currency patterns), and `formatDate`'s long-style month names are always English. An invalid locale string is a **Tier-2 panic** (`intl.X: invalid locale '...'`), since locales are normally literals.

### formatNumber

Formats a number using the locale's grouping and decimal symbols (ICU `FixedDecimalFormatter`).

- **Parameters:** `n` (number) — must be finite; `locale` (string) — BCP-47 locale.
- **Returns:** string.

A non-finite number (NaN/Inf) is a Tier-2 panic.

```ascript
intl.formatNumber(1234567.89, "en-US")   // "1,234,567.89"
intl.formatNumber(1234567.89, "de-DE")   // "1.234.567,89"
```

### formatCurrency

Formats a monetary amount: the number is rendered with the locale's grouping at the currency's standard fraction digits, then prefixed with a currency symbol.

- **Parameters:** `amount` (number) — must be finite; `code` (string) — currency code such as `"USD"`; `locale` (string) — BCP-47 locale.
- **Returns:** string.

Fraction digits follow the currency (2 for most, 0 for JPY/KRW). Known symbols include `$` (USD), `€` (EUR), `£` (GBP), `¥` (JPY/CNY), `₩` (KRW), `₹` (INR), and `CHF `; an unknown code falls back to the code itself as a prefix. Symbol placement is always prefix — a documented simplification of real CLDR currency patterns.

```ascript
intl.formatCurrency(1234.5, "USD", "en-US")   // "$1,234.50"
intl.formatCurrency(1234, "JPY", "ja-JP")     // "¥1,234"
```

### formatDate

Renders a `std/date` instant in a locale-appropriate style (pragmatic fallback).

- **Parameters:**
  - `instant` (instant) — a `std/date` instant object (its `epochMs` field is read; rendered in UTC).
  - `locale` (string) — BCP-47 locale.
  - `style` (string, optional) — `"short"`, `"medium"` (default), or `"long"`.
- **Returns:** string.

The field order is derived from the locale's region (US → month-day-year; CJK → year-month-day; most others → day-month-year). Long-style month names are English.

```ascript
let inst = date.fromEpochMs(1623760200000)   // 2021-06-15T12:30:00Z
intl.formatDate(inst, "en-US", "medium")     // "Jun 15, 2021"
intl.formatDate(inst, "de-DE", "medium")     // "15 Jun 2021"
intl.formatDate(inst, "ja-JP", "short")      // "2021/06/15"
```

### caseUpper

Locale-sensitive uppercase mapping (ICU `CaseMapper`).

- **Parameters:** `text` (string); `locale` (string) — BCP-47 locale.
- **Returns:** string.

The locale matters: Turkish maps dotless/dotted I differently from English.

```ascript
intl.caseUpper("istanbul", "tr")   // "İSTANBUL"
intl.caseUpper("istanbul", "en")   // "ISTANBUL"
```

### caseLower

Locale-sensitive lowercase mapping (ICU `CaseMapper`).

- **Parameters:** `text` (string); `locale` (string) — BCP-47 locale.
- **Returns:** string.

```ascript
intl.caseLower("HELLO", "en")   // "hello"
```

### compare

Locale-aware string collation (ICU `Collator`).

- **Parameters:** `a` (string); `b` (string); `locale` (string) — BCP-47 locale.
- **Returns:** number — `-1` if `a` sorts before `b`, `0` if equal, `1` if after.

```ascript
intl.compare("apple", "banana", "en")   // -1
intl.compare("x", "x", "en")            //  0
intl.compare("b", "a", "en")            //  1
```
