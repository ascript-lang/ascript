// datetime_intl.as
// ---------------------------------------------------------------------------
// Time, dates, and locale-aware formatting:
//   std/time : monotonic() for elapsed measurement, sleep(ms) (async)
//   std/date : now(), parse(), format(), addDays(), diffMs()
//   std/intl : formatNumber / formatCurrency / caseUpper (locale-aware)
//
// Instant objects (from date.now/parse/add*) expose fields like .year, .month,
// .day, .iso, .epochMs. date.parse returns [instant, err]; format/add*/diffMs
// return their value directly. The intl.* helpers return strings directly.
// ---------------------------------------------------------------------------

import * as time from "std/time"
import * as date from "std/date"
import * as intl from "std/intl"

async fn main() {
  // --- monotonic timing around an async sleep ---------------------------
  print("=== Timing ===")
  let start = time.monotonic()
  await time.sleep(5)            // sleep 5ms
  let elapsed = time.monotonic() - start
  // Elapsed will be >= 5ms; print it as a whole number of ms for stability.
  print(`slept ~5ms, measured ${elapsed >= 5} (>=5ms): ${elapsed} ms`)

  // --- current instant --------------------------------------------------
  print("\n=== Dates ===")
  let nowInst = date.now()
  print(`now.year = ${nowInst.year}, now.iso = ${nowInst.iso}`)

  // --- parse a fixed ISO timestamp (deterministic for the rest) ---------
  let [inst, parseErr] = date.parse("2021-06-15T12:30:00Z")
  if (parseErr != nil) {
    print(`parse failed: ${parseErr.message}`)
    return
  }
  print(`parsed   = ${inst.iso}  (y=${inst.year} m=${inst.month} d=${inst.day})`)

  // --- format with a custom strftime-style pattern ----------------------
  let formatted = date.format(inst, "%Y/%m/%d")
  print(`format   = ${formatted}`)

  // --- date arithmetic + difference -------------------------------------
  let plusWeek = date.addDays(inst, 7)
  print(`+7 days  = ${plusWeek.iso}`)
  let diff = date.diffMs(plusWeek, inst)   // (a - b) in milliseconds
  print(`diffMs   = ${diff} ms  (== ${diff / 86400000} days)`)

  // --- locale-aware number / currency / case formatting -----------------
  print("\n=== Intl ===")
  // Same number, different grouping conventions per locale.
  print(`number en-US = ${intl.formatNumber(1234567, "en-US")}`)
  print(`number de-DE = ${intl.formatNumber(1234567, "de-DE")}`)
  print(`currency USD = ${intl.formatCurrency(1234.5, "USD", "en-US")}`)
  // Turkish-locale upper-casing: dotted/dotless i handled per language rules.
  print(`upper(tr)    = ${intl.caseUpper("istanbul", "tr")}`)
  print(`upper(en)    = ${intl.caseUpper("istanbul", "en")}`)

  // Locale-correct long month names (SP5 §8) — the month NAME differs per locale,
  // not just the field order.
  print(`date en-US (long) = ${intl.formatDate(inst, "en-US", "long")}`)
  print(`date de-DE (long) = ${intl.formatDate(inst, "de-DE", "long")}`)
  print(`date fr-FR (long) = ${intl.formatDate(inst, "fr-FR", "long")}`)
  print(`date ja-JP (long) = ${intl.formatDate(inst, "ja-JP", "long")}`)
}

await main()
