//! `std/telemetry` data model: the configured pipeline (`TelemetryState`), the
//! exporter descriptors, the buffered signals (spans/metrics/events), and the
//! injectable HTTP-send seam.
//!
//! The whole module is `#[cfg(feature = "telemetry")]`-gated; the SP11-facing
//! `Interp::telemetry_*` soft hook lives in `interp.rs` with always-present
//! signatures (its body bridges to this module only when the feature is on), so
//! `std/ai` compiles with telemetry absent.

use crate::value::Value;
use indexmap::IndexMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// A captured outbound HTTP request — what the exporters would have POSTed. In
/// tests (and any `OutputSink::Capture` interp) the send seam records these
/// instead of opening a socket, so unit tests assert the exact wire payloads
/// (`Interp::telemetry_capture()`).
#[derive(Clone, Debug, PartialEq)]
pub struct CapturedRequest {
    /// Which exporter produced it (`"otlp"`, `"sentry"`, `"posthog"`).
    pub exporter: &'static str,
    /// The signal class (`"traces"`, `"metrics"`, `"logs"`, `"events"`).
    pub signal: &'static str,
    /// Full request URL.
    pub url: String,
    /// Request headers (name → value), in insertion order.
    pub headers: Vec<(String, String)>,
    /// The request body (JSON for OTLP/PostHog; newline-delimited for a Sentry
    /// envelope).
    pub body: String,
}

/// A flattened, hex-id view of a buffered span for tests (read via
/// `Interp::telemetry_spans_debug`). Lets F1 assert tracing semantics (name,
/// parenting, status, attrs, events) WITHOUT depending on the F2 OTLP wire
/// shaping.
#[derive(Clone, Debug)]
pub struct SpanSnapshot {
    pub name: String,
    pub trace_id: String,
    pub span_id: String,
    pub parent_id: Option<String>,
    pub status_code: u8,
    pub status_message: Option<String>,
    pub attributes: Vec<(String, String)>,
    pub events: Vec<String>,
}

/// One finished span, exporter-agnostic. Exporters shape it into OTLP / Sentry
/// JSON on flush.
#[derive(Clone, Debug)]
pub struct SpanRecord {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    /// Parent span id within the same trace, or `None` for a root span.
    pub parent_id: Option<[u8; 8]>,
    pub name: String,
    pub start_unix_nano: u128,
    pub end_unix_nano: u128,
    pub attributes: Vec<(String, Value)>,
    pub events: Vec<SpanEvent>,
    pub status: SpanStatusRecord,
}

impl SpanRecord {
    /// A flattened, hex-id [`SpanSnapshot`] for tests.
    pub fn snapshot(&self) -> SpanSnapshot {
        SpanSnapshot {
            name: self.name.clone(),
            trace_id: hex(&self.trace_id),
            span_id: hex(&self.span_id),
            parent_id: self.parent_id.map(|p| hex(&p)),
            status_code: self.status.code.otlp_code(),
            status_message: self.status.message.clone(),
            attributes: self
                .attributes
                .iter()
                .map(|(k, v)| (k.clone(), v.to_string()))
                .collect(),
            events: self.events.iter().map(|e| e.name.clone()).collect(),
        }
    }
}

/// An in-progress span, held in `ResourceState::TelemetrySpan` between
/// `startSpan` and `end`. On `end` it is frozen into a [`SpanRecord`].
#[derive(Clone, Debug)]
pub struct OpenSpan {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub parent_id: Option<[u8; 8]>,
    pub name: String,
    pub start_unix_nano: u128,
    pub attributes: Vec<(String, Value)>,
    pub events: Vec<SpanEvent>,
    pub status: SpanStatusRecord,
}

impl OpenSpan {
    /// Freeze this open span into a finished [`SpanRecord`] with an end timestamp.
    pub fn finish(self, end_unix_nano: u128) -> SpanRecord {
        SpanRecord {
            trace_id: self.trace_id,
            span_id: self.span_id,
            parent_id: self.parent_id,
            name: self.name,
            start_unix_nano: self.start_unix_nano,
            end_unix_nano,
            attributes: self.attributes,
            events: self.events,
            status: self.status,
        }
    }
}

/// A timestamped event recorded on a span (`span.addEvent(name, attrs?)`).
#[derive(Clone, Debug)]
pub struct SpanEvent {
    pub name: String,
    pub time_unix_nano: u128,
    pub attributes: Vec<(String, Value)>,
}

/// A span's outcome, with an optional human message (recorded by `setStatus` or
/// a recovered panic in `telemetry.span`).
#[derive(Clone, Debug, Default)]
pub struct SpanStatusRecord {
    pub code: SpanStatusCode,
    pub message: Option<String>,
}

/// OTLP status codes (`0` unset, `1` ok, `2` error). Mirrors the public
/// `crate::interp::SpanStatus` (which is non-feature-gated for the SP11 hook).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpanStatusCode {
    #[default]
    Unset,
    Ok,
    Error,
}

impl SpanStatusCode {
    /// The OTLP numeric code.
    pub fn otlp_code(self) -> u8 {
        match self {
            SpanStatusCode::Unset => 0,
            SpanStatusCode::Ok => 1,
            SpanStatusCode::Error => 2,
        }
    }
}

/// The kind of a metric instrument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetricKind {
    Counter,
    Histogram,
    Gauge,
}

/// One accumulated metric data point, keyed by its attribute set. Cumulative
/// temporality (v1): a counter/sum keeps a running total; a gauge keeps the last
/// value; a histogram keeps count/sum/bucket-less aggregate.
#[derive(Clone, Debug, Default)]
pub struct MetricPoint {
    /// Sum (counter) or last value (gauge) or running sum of recorded values
    /// (histogram).
    pub value: f64,
    /// Number of recorded samples (histogram `count`; unused for sum/gauge).
    pub count: u64,
    /// Min/max over recorded samples (histogram only).
    pub min: f64,
    pub max: f64,
}

/// A registered metric instrument: name + kind + unit/description + per-attribute
/// accumulated points. The attribute key is the canonical serialization of the
/// attribute set (sorted `k=v` joined by `;`).
#[derive(Clone, Debug)]
pub struct MetricInstrument {
    pub name: String,
    pub kind: MetricKind,
    pub unit: Option<String>,
    pub description: Option<String>,
    /// Points keyed by canonical attribute-set string → (attributes, point).
    pub points: IndexMap<String, (Vec<(String, Value)>, MetricPoint)>,
    /// Wall-clock start of the cumulative window (set at instrument creation).
    pub start_unix_nano: u128,
}

/// One analytics event captured via `telemetry.capture` / `telemetry.identify`.
#[derive(Clone, Debug)]
pub struct AnalyticsEvent {
    pub event: String,
    pub distinct_id: String,
    pub properties: Vec<(String, Value)>,
    /// For `identify`: the `$set` person properties.
    pub set_props: Vec<(String, Value)>,
    pub time_unix_nano: u128,
}

/// One entry on the interp's current-span stack: the live span's resource id
/// plus its trace/span ids (so a child can parent to it without re-reading the
/// resource table). Pushed for the duration of a `telemetry.span` callback.
#[derive(Clone, Copy, Debug)]
pub struct SpanCtx {
    pub resource_id: u64,
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
}

/// An OTLP exporter descriptor (`telemetry.otlp({...})`).
#[derive(Clone, Debug)]
pub struct OtlpExporter {
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
}

/// A Sentry exporter descriptor — the parsed DSN.
#[derive(Clone, Debug)]
pub struct SentryExporter {
    /// The envelope ingest URL (`https://o…/api/<project>/envelope/`).
    pub envelope_url: String,
    /// The public key (DSN user component), used in the auth header.
    pub public_key: String,
}

/// A PostHog exporter descriptor.
#[derive(Clone, Debug)]
pub struct PostHogExporter {
    pub host: String,
    pub api_key: String,
}

/// The set of configured exporters.
#[derive(Clone, Debug, Default)]
pub struct Exporters {
    pub otlp: Option<OtlpExporter>,
    pub sentry: Option<SentryExporter>,
    pub posthog: Option<PostHogExporter>,
}

/// The configured, live telemetry pipeline (set by `telemetry.init`, cleared by
/// `shutdown`). `None` on `Interp` means uninitialized = every call is a no-op.
pub struct TelemetryState {
    /// `service.name` resource attribute (required).
    pub service: String,
    pub version: Option<String>,
    pub env: Option<String>,
    /// Extra resource attributes.
    pub resource: Vec<(String, Value)>,
    pub exporters: Exporters,
    /// Additionally emit each `capture` as an OTLP log record.
    pub mirror_events_to_otlp: bool,
    /// Buffered finished spans awaiting flush.
    pub spans: Vec<SpanRecord>,
    /// Registered metric instruments, by resource id of the handle.
    pub instruments: IndexMap<u64, MetricInstrument>,
    /// Buffered analytics events awaiting flush.
    pub events: Vec<AnalyticsEvent>,
}

impl TelemetryState {
    /// Resource attributes as OTLP-ready key/value pairs, with `service.name`,
    /// `service.version`, and `deployment.environment.name` folded in.
    pub fn resource_attributes(&self) -> Vec<(String, Value)> {
        let mut out: Vec<(String, Value)> = Vec::new();
        out.push(("service.name".to_string(), Value::Str(self.service.as_str().into())));
        if let Some(v) = &self.version {
            out.push(("service.version".to_string(), Value::Str(v.as_str().into())));
        }
        if let Some(e) = &self.env {
            out.push((
                "deployment.environment.name".to_string(),
                Value::Str(e.as_str().into()),
            ));
        }
        for (k, v) in &self.resource {
            out.push((k.clone(), v.clone()));
        }
        out
    }
}

/// Current wall-clock time in nanoseconds since the Unix epoch. Used for span
/// start/end and event timestamps. `SystemTime` can in principle be before the
/// epoch on a misconfigured clock; we saturate to 0 rather than panic.
pub fn now_unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Lowercase hex encoding of a byte id (OTLP trace/span ids are hex strings).
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

use std::cell::Cell;

thread_local! {
    /// A small xorshift64* PRNG state for minting trace/span ids. Seeded lazily
    /// from the wall clock + a per-thread address so ids are unique without a
    /// crypto dependency (OTLP ids need not be cryptographically random, just
    /// collision-free within a trace). The interp is single-threaded, so a
    /// thread-local is sufficient.
    static RNG: Cell<u64> = const { Cell::new(0) };
}

fn next_rand() -> u64 {
    RNG.with(|cell| {
        let mut x = cell.get();
        if x == 0 {
            // Lazy seed: clock nanos mixed with a stack address. Never 0.
            let seed = now_unix_nanos() as u64 ^ ((&x as *const u64 as u64).rotate_left(17));
            x = seed | 1;
        }
        // xorshift64*
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        cell.set(x);
        x.wrapping_mul(0x2545F4914F6CDD1D)
    })
}

/// Mint a fresh 16-byte trace id (never all-zero — OTLP requires a non-zero id).
pub fn new_trace_id() -> [u8; 16] {
    loop {
        let hi = next_rand().to_be_bytes();
        let lo = next_rand().to_be_bytes();
        let mut id = [0u8; 16];
        id[..8].copy_from_slice(&hi);
        id[8..].copy_from_slice(&lo);
        if id != [0u8; 16] {
            return id;
        }
    }
}

/// Mint a fresh 8-byte span id (never all-zero).
pub fn new_span_id() -> [u8; 8] {
    loop {
        let id = next_rand().to_be_bytes();
        if id != [0u8; 8] {
            return id;
        }
    }
}

/// Canonical key for a metric attribute set: sorted `k=display(v)` joined by `;`.
/// Two `add` calls with the same attributes accumulate into the same point.
pub fn attr_key(attrs: &[(String, Value)]) -> String {
    let mut parts: Vec<String> = attrs.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
    parts.sort();
    parts.join(";")
}
