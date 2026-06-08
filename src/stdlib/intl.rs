//! `std/intl` — locale-aware formatting, case folding, and collation, backed by
//! a pragmatic subset of ICU (icu 1.5, `compiled_data`). BCP-47 locale strings
//! (`"en-US"`, `"de-DE"`, `"tr"`). An invalid locale is a Tier-2 panic
//! (`intl.X: invalid locale '...'`) since locales are normally literals.
//!
//! ## ICU APIs used vs. pragmatic fallbacks
//!
//! - `formatNumber`: ICU `FixedDecimalFormatter` (`icu::decimal`) with the
//!   locale's grouping/decimal symbols. The `FixedDecimal` is built from the
//!   number's decimal string (not `try_from_f64`, which needs fixed_decimal's
//!   `ryu` feature that icu does not enable) — robust and dependency-free.
//! - `caseUpper` / `caseLower`: ICU `CaseMapper` (`icu::casemap`), full
//!   language-sensitive case mapping (e.g. Turkish dotted/dotless I).
//! - `compare`: ICU `Collator` (`icu::collator`) → `Ordering` → -1/0/1.
//! - `formatCurrency`: PRAGMATIC FALLBACK. Stable icu 1.5 has no simple currency
//!   formatter (currency lives in `icu_experimental`). We format the amount with
//!   the locale's `FixedDecimalFormatter` at the currency's standard fraction
//!   digits (2 for most, 0 for JPY/KRW), then prefix a symbol from a small table
//!   (USD→$, EUR→€, GBP→£, JPY→¥, …; unknown → the code). Symbol placement is
//!   always prefix — a documented simplification of real CLDR currency patterns.
//! - `formatDate`: PRAGMATIC FALLBACK. icu 1.5's `DateTime`/neo formatter input
//!   plumbing (constructing an `icu::calendar` Date/Time from epoch-ms and
//!   threading a `length::Date` style) is heavyweight for a one-shot "format
//!   this instant in this locale at this style". We instead derive the locale's
//!   region and render via chrono with a small per-region, per-style pattern map
//!   (e.g. en-US `MDY`, most others `DMY`, ja `YMD`), defaulting to ISO-ish.
//!   Long/medium **month and weekday names are locale-correct** (SP5 §8): they
//!   come from a curated CLDR-derived table keyed by `loc.id.language`
//!   (`MONTH_NAMES` / `MONTH_ABBR`), covering en/de/fr/es/it/pt/nl/ru/tr (plus
//!   ja/zh/ko, which use the numeric 年月日 form). An unlisted language falls
//!   back to English. The instant is the std/date object (read via `epochMs`).

use super::{arg, bi, want_number, want_object, want_string};
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::Value;
use fixed_decimal::FixedDecimal;
use icu::casemap::CaseMapper;
use icu::collator::Collator;
use icu::decimal::FixedDecimalFormatter;
use icu::locid::Locale;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("formatNumber", bi("intl.formatNumber")),
        ("formatCurrency", bi("intl.formatCurrency")),
        ("formatDate", bi("intl.formatDate")),
        ("caseUpper", bi("intl.caseUpper")),
        ("caseLower", bi("intl.caseLower")),
        ("compare", bi("intl.compare")),
    ]
}

/// Parse a BCP-47 locale string; an invalid locale is a Tier-2 panic.
fn parse_locale(s: &str, span: Span, ctx: &str) -> Result<Locale, Control> {
    s.parse::<Locale>()
        .map_err(|_| AsError::at(format!("{}: invalid locale '{}'", ctx, s), span).into())
}

/// Build a `FixedDecimal` from an f64 via its decimal string representation.
/// Avoids fixed_decimal's `ryu`-gated `try_from_f64`. `fraction_digits`, when
/// `Some(d)`, pads/truncates to exactly `d` digits (used by formatCurrency).
fn fixed_from_f64(n: f64, fraction_digits: Option<u8>) -> FixedDecimal {
    let s = match fraction_digits {
        Some(d) => format!("{:.*}", d as usize, n),
        // {} on f64 gives the shortest round-trip decimal (no exponent for
        // typical magnitudes), which FixedDecimal parses directly.
        None => format!("{}", n),
    };
    // The string we produce is always a valid decimal; fall back to 0 only if
    // a non-finite value (NaN/inf) slipped through.
    s.parse::<FixedDecimal>()
        .unwrap_or_else(|_| FixedDecimal::from(0))
}

/// Currency symbol + standard fraction digits for a currency code.
fn currency_info(code: &str) -> (String, u8) {
    match code {
        "USD" => ("$".into(), 2),
        "EUR" => ("€".into(), 2),
        "GBP" => ("£".into(), 2),
        "JPY" => ("¥".into(), 0),
        "CNY" => ("¥".into(), 2),
        "KRW" => ("₩".into(), 0),
        "INR" => ("₹".into(), 2),
        "CHF" => ("CHF ".into(), 2),
        _ => (format!("{} ", code), 2),
    }
}

/// Region-aware date pattern for the pragmatic formatDate fallback.
fn date_pattern(loc: &Locale, style: &str) -> &'static str {
    let lang = loc.id.language;
    let lang = lang.as_str();
    let order_ymd = lang == "ja" || lang == "zh" || lang == "ko";
    let order_mdy = match loc.id.region {
        Some(r) => r.as_str() == "US",
        None => false,
    };
    match style {
        "short" => {
            if order_ymd {
                "%Y/%m/%d"
            } else if order_mdy {
                "%-m/%-d/%y"
            } else {
                "%d/%m/%Y"
            }
        }
        "long" => {
            if order_ymd {
                "%Y\u{5e74}%-m\u{6708}%-d\u{65e5}" // YYYY年M月D日
            } else if order_mdy {
                "%B %-d, %Y"
            } else {
                "%-d %B %Y"
            }
        }
        // "medium" (default)
        _ => {
            if order_ymd {
                "%Y/%m/%d"
            } else if order_mdy {
                "%b %-d, %Y"
            } else {
                "%d %b %Y"
            }
        }
    }
}

/// Curated CLDR-derived long month names (index 0 = January) per language. Covers
/// the locales the intl corpus exercises plus the common Western European set; an
/// unlisted language falls back to English (`en`).
fn long_month_names(lang: &str) -> [&'static str; 12] {
    match lang {
        "de" => [
            "Januar", "Februar", "März", "April", "Mai", "Juni", "Juli", "August", "September",
            "Oktober", "November", "Dezember",
        ],
        "fr" => [
            "janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre",
            "octobre", "novembre", "décembre",
        ],
        "es" => [
            "enero", "febrero", "marzo", "abril", "mayo", "junio", "julio", "agosto", "septiembre",
            "octubre", "noviembre", "diciembre",
        ],
        "it" => [
            "gennaio", "febbraio", "marzo", "aprile", "maggio", "giugno", "luglio", "agosto",
            "settembre", "ottobre", "novembre", "dicembre",
        ],
        "pt" => [
            "janeiro", "fevereiro", "março", "abril", "maio", "junho", "julho", "agosto",
            "setembro", "outubro", "novembro", "dezembro",
        ],
        "nl" => [
            "januari", "februari", "maart", "april", "mei", "juni", "juli", "augustus",
            "september", "oktober", "november", "december",
        ],
        "ru" => [
            "января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября",
            "октября", "ноября", "декабря",
        ],
        "tr" => [
            "Ocak", "Şubat", "Mart", "Nisan", "Mayıs", "Haziran", "Temmuz", "Ağustos", "Eylül",
            "Ekim", "Kasım", "Aralık",
        ],
        // English (and the fallback for any unlisted language).
        _ => [
            "January", "February", "March", "April", "May", "June", "July", "August", "September",
            "October", "November", "December",
        ],
    }
}

/// Curated abbreviated month names (index 0 = January) per language, for the
/// "medium" style. Unlisted languages fall back to English.
fn abbr_month_names(lang: &str) -> [&'static str; 12] {
    match lang {
        "de" => [
            "Jan.", "Feb.", "März", "Apr.", "Mai", "Juni", "Juli", "Aug.", "Sept.", "Okt.",
            "Nov.", "Dez.",
        ],
        "fr" => [
            "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.",
            "nov.", "déc.",
        ],
        "es" => [
            "ene.", "feb.", "mar.", "abr.", "may.", "jun.", "jul.", "ago.", "sept.", "oct.",
            "nov.", "dic.",
        ],
        "it" => [
            "gen.", "feb.", "mar.", "apr.", "mag.", "giu.", "lug.", "ago.", "set.", "ott.",
            "nov.", "dic.",
        ],
        "pt" => [
            "jan.", "fev.", "mar.", "abr.", "mai.", "jun.", "jul.", "ago.", "set.", "out.",
            "nov.", "dez.",
        ],
        "nl" => [
            "jan.", "feb.", "mrt.", "apr.", "mei", "jun.", "jul.", "aug.", "sep.", "okt.", "nov.",
            "dec.",
        ],
        "ru" => [
            "янв.", "февр.", "мар.", "апр.", "мая", "июн.", "июл.", "авг.", "сент.", "окт.",
            "нояб.", "дек.",
        ],
        "tr" => [
            "Oca", "Şub", "Mar", "Nis", "May", "Haz", "Tem", "Ağu", "Eyl", "Eki", "Kas", "Ara",
        ],
        _ => [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ],
    }
}

/// Render a date for the long/medium style with a locale-correct month name
/// substituted, for the Western DMY/MDY orders. `ymd` locales (ja/zh/ko) keep
/// the numeric 年月日 pattern and never reach this path.
fn render_named_date(dt: &chrono::NaiveDate, lang: &str, mdy: bool, long: bool) -> String {
    use chrono::Datelike;
    let month_idx = (dt.month0()) as usize; // 0..=11
    let name = if long {
        long_month_names(lang)[month_idx]
    } else {
        abbr_month_names(lang)[month_idx]
    };
    let day = dt.day();
    let year = dt.year();
    if mdy {
        // English-style "Month D, YYYY".
        format!("{} {}, {}", name, day, year)
    } else {
        // Most locales: "D Month YYYY".
        format!("{} {} {}", day, name, year)
    }
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("intl.{}", f);
    match func {
        "formatNumber" => {
            let n = want_number(&arg(args, 0), span, &ctx("formatNumber"))?;
            if !n.is_finite() {
                return Err(AsError::at(
                    format!(
                        "intl.formatNumber: cannot format a non-finite number ({})",
                        n
                    ),
                    span,
                )
                .into());
            }
            let loc = parse_locale(
                &want_string(&arg(args, 1), span, &ctx("formatNumber"))?,
                span,
                &ctx("formatNumber"),
            )?;
            let fdf = FixedDecimalFormatter::try_new(&loc.into(), Default::default()).map_err(
                |e| -> Control { AsError::at(format!("intl.formatNumber: {}", e), span).into() },
            )?;
            let fd = fixed_from_f64(n, None);
            Ok(Value::Str(fdf.format_to_string(&fd).into()))
        }
        "formatCurrency" => {
            let n = want_number(&arg(args, 0), span, &ctx("formatCurrency"))?;
            if !n.is_finite() {
                return Err(AsError::at(
                    format!(
                        "intl.formatCurrency: cannot format a non-finite number ({})",
                        n
                    ),
                    span,
                )
                .into());
            }
            let code = want_string(&arg(args, 1), span, &ctx("formatCurrency"))?;
            let loc = parse_locale(
                &want_string(&arg(args, 2), span, &ctx("formatCurrency"))?,
                span,
                &ctx("formatCurrency"),
            )?;
            let (symbol, digits) = currency_info(&code);
            let fdf = FixedDecimalFormatter::try_new(&loc.into(), Default::default()).map_err(
                |e| -> Control { AsError::at(format!("intl.formatCurrency: {}", e), span).into() },
            )?;
            let fd = fixed_from_f64(n, Some(digits));
            Ok(Value::Str(
                format!("{}{}", symbol, fdf.format_to_string(&fd)).into(),
            ))
        }
        "formatDate" => {
            let o = want_object(&arg(args, 0), span, &ctx("formatDate"))?;
            let epoch_ms = {
                let b = o.borrow();
                match b.get("epochMs") {
                    Some(Value::Float(n)) => *n as i64,
                    _ => {
                        return Err(AsError::at(
                            "intl.formatDate expects an instant object (with epochMs)",
                            span,
                        )
                        .into())
                    }
                }
            };
            let loc = parse_locale(
                &want_string(&arg(args, 1), span, &ctx("formatDate"))?,
                span,
                &ctx("formatDate"),
            )?;
            let style = match args.get(2) {
                None | Some(Value::Nil) => "medium".to_string(),
                Some(v) => want_string(v, span, &ctx("formatDate"))?.to_string(),
            };
            use chrono::TimeZone;
            let dt = chrono::Utc
                .timestamp_millis_opt(epoch_ms)
                .single()
                .unwrap_or_else(|| chrono::Utc.timestamp_millis_opt(0).unwrap());
            let naive = dt.naive_utc();
            // Determine the locale's ordering once.
            let lang = loc.id.language.as_str().to_string();
            let order_ymd = lang == "ja" || lang == "zh" || lang == "ko";
            let order_mdy = loc.id.region.map(|r| r.as_str() == "US").unwrap_or(false);
            // long/medium (non-YMD) substitute a locale-correct month NAME; YMD
            // and short styles keep the numeric pattern (locale-correct already).
            let out = if !order_ymd && (style == "long" || style == "medium") {
                render_named_date(&naive.date(), &lang, order_mdy, style == "long")
            } else {
                let pattern = date_pattern(&loc, &style);
                naive.format(pattern).to_string()
            };
            Ok(Value::Str(out.into()))
        }
        "caseUpper" | "caseLower" => {
            let s = want_string(&arg(args, 0), span, &ctx(func))?;
            let loc = parse_locale(
                &want_string(&arg(args, 1), span, &ctx(func))?,
                span,
                &ctx(func),
            )?;
            let cm = CaseMapper::new();
            let out = if func == "caseUpper" {
                cm.uppercase_to_string(&s, &loc.id)
            } else {
                cm.lowercase_to_string(&s, &loc.id)
            };
            Ok(Value::Str(out.into()))
        }
        "compare" => {
            let a = want_string(&arg(args, 0), span, &ctx("compare"))?;
            let b = want_string(&arg(args, 1), span, &ctx("compare"))?;
            let loc = parse_locale(
                &want_string(&arg(args, 2), span, &ctx("compare"))?,
                span,
                &ctx("compare"),
            )?;
            let collator =
                Collator::try_new(&loc.into(), Default::default()).map_err(|e| -> Control {
                    AsError::at(format!("intl.compare: {}", e), span).into()
                })?;
            let ord = collator.compare(&a, &b);
            Ok(Value::Float(match ord {
                std::cmp::Ordering::Less => -1.0,
                std::cmp::Ordering::Equal => 0.0,
                std::cmp::Ordering::Greater => 1.0,
            }))
        }
        _ => Err(AsError::at(format!("std/intl has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span {
        Span::new(0, 0)
    }
    fn s(x: &str) -> Value {
        Value::Str(x.into())
    }
    fn n(x: f64) -> Value {
        Value::Float(x)
    }
    fn str_of(v: Value) -> String {
        match v {
            Value::Str(s) => s.to_string(),
            other => panic!("expected string, got {:?}", other),
        }
    }

    #[test]
    fn format_number_locale_grouping_differs() {
        let en = str_of(call("formatNumber", &[n(1234567.0), s("en-US")], sp()).unwrap());
        let de = str_of(call("formatNumber", &[n(1234567.0), s("de-DE")], sp()).unwrap());
        assert_eq!(en, "1,234,567");
        assert_eq!(de, "1.234.567");
    }

    #[test]
    fn format_number_decimals() {
        let en = str_of(call("formatNumber", &[n(1234567.89), s("en-US")], sp()).unwrap());
        let de = str_of(call("formatNumber", &[n(1234567.89), s("de-DE")], sp()).unwrap());
        assert_eq!(en, "1,234,567.89");
        assert_eq!(de, "1.234.567,89");
    }

    #[test]
    fn case_upper_turkish_dotted_i() {
        let tr = str_of(call("caseUpper", &[s("istanbul"), s("tr")], sp()).unwrap());
        let en = str_of(call("caseUpper", &[s("istanbul"), s("en")], sp()).unwrap());
        assert_eq!(tr, "İSTANBUL");
        assert_eq!(en, "ISTANBUL");
    }

    #[test]
    fn case_lower() {
        let en = str_of(call("caseLower", &[s("HELLO"), s("en")], sp()).unwrap());
        assert_eq!(en, "hello");
    }

    #[test]
    fn compare_collation() {
        assert_eq!(
            call("compare", &[s("apple"), s("banana"), s("en")], sp()).unwrap(),
            n(-1.0)
        );
        assert_eq!(
            call("compare", &[s("b"), s("a"), s("en")], sp()).unwrap(),
            n(1.0)
        );
        assert_eq!(
            call("compare", &[s("x"), s("x"), s("en")], sp()).unwrap(),
            n(0.0)
        );
    }

    #[test]
    fn format_currency() {
        let usd = str_of(call("formatCurrency", &[n(1234.5), s("USD"), s("en-US")], sp()).unwrap());
        let jpy = str_of(call("formatCurrency", &[n(1234.0), s("JPY"), s("ja-JP")], sp()).unwrap());
        assert_eq!(usd, "$1,234.50");
        assert_eq!(jpy, "¥1,234");
    }

    #[test]
    fn format_date_styles() {
        // 2021-06-15T12:30:00Z = 1623760200000 ms. Build a minimal instant
        // object (only epochMs is read by formatDate) so this test doesn't
        // depend on the `datetime` feature being enabled.
        let mut o: indexmap::IndexMap<String, Value> = indexmap::IndexMap::new();
        o.insert("epochMs".into(), Value::Float(1623760200000.0));
        let inst = Value::Object(crate::value::ObjectCell::new(o));
        let us =
            str_of(call("formatDate", &[inst.clone(), s("en-US"), s("medium")], sp()).unwrap());
        let de =
            str_of(call("formatDate", &[inst.clone(), s("de-DE"), s("medium")], sp()).unwrap());
        let ja = str_of(call("formatDate", &[inst, s("ja-JP"), s("short")], sp()).unwrap());
        assert_eq!(us, "Jun 15, 2021");
        assert_eq!(de, "15 Juni 2021"); // German abbreviated June == "Juni"
        assert_eq!(ja, "2021/06/15");
    }

    // SP5 §8: long/medium month names are locale-correct (not English).
    #[test]
    fn format_date_long_month_names_locale_correct() {
        // 2021-03-15T12:00:00Z = 1615809600000 ms (March, to exercise März/mars).
        let inst = || {
            let mut o: indexmap::IndexMap<String, Value> = indexmap::IndexMap::new();
            o.insert("epochMs".into(), Value::Float(1615809600000.0));
            Value::Object(crate::value::ObjectCell::new(o))
        };
        let de = str_of(call("formatDate", &[inst(), s("de-DE"), s("long")], sp()).unwrap());
        let fr = str_of(call("formatDate", &[inst(), s("fr-FR"), s("long")], sp()).unwrap());
        let en = str_of(call("formatDate", &[inst(), s("en-US"), s("long")], sp()).unwrap());
        let ja = str_of(call("formatDate", &[inst(), s("ja-JP"), s("long")], sp()).unwrap());
        assert_eq!(de, "15 März 2021", "German long month");
        assert_eq!(fr, "15 mars 2021", "French long month");
        assert_eq!(en, "March 15, 2021", "English long month (MDY)");
        assert_eq!(ja, "2021\u{5e74}3\u{6708}15\u{65e5}", "Japanese keeps 年月日");
        // The previously-English locales now differ from English.
        assert_ne!(de, en);
        assert_ne!(fr, en);
    }

    #[test]
    fn non_finite_number_panics() {
        let inf = Value::Float(f64::INFINITY);
        assert!(matches!(
            call("formatNumber", &[inf, s("en-US")], sp()),
            Err(Control::Panic(_))
        ));
        let nan = Value::Float(f64::NAN);
        assert!(matches!(
            call("formatCurrency", &[nan, s("USD"), s("en-US")], sp()),
            Err(Control::Panic(_))
        ));
    }

    #[test]
    fn invalid_locale_panics() {
        assert!(matches!(
            call("formatNumber", &[n(1.0), s("!!!")], sp()),
            Err(Control::Panic(_))
        ));
    }
}
