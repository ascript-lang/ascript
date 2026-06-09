//! `std/net` — general networking utilities (feature `net`), spec §5a phase 5.
//!
//! - `net.lookup(host) -> [array<string>, err]` — Tier-1. Resolves a hostname to a
//!   de-duplicated list of IP-address strings via `tokio::net::lookup_host`. A bare
//!   hostname without a port (e.g. `"localhost"`) has `":0"` appended before
//!   resolution; the returned strings contain only the IP (port stripped). Async.
//! - `net.lookupOne(host) -> [string, err]` — the first resolved IP, or err if the
//!   lookup fails or returns zero results.

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control, Interp};
use crate::span::Span;
use crate::value::Value;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("lookup", bi("net.lookup")),
        ("lookupOne", bi("net.lookupOne")),
    ]
}

fn err_pair(msg: String) -> Value {
    make_pair(Value::Nil, make_error(Value::Str(msg.into())))
}

/// Normalise `host` into a `host:port` form for `tokio::net::lookup_host`.
/// If the input already contains a colon that is not the start of an IPv6
/// bracket expression, we preserve it; otherwise we append `:0`.
fn to_addr(host: &str) -> String {
    // Already bracketed IPv6 (e.g. "[::1]:80"): leave as-is.
    if host.starts_with('[') {
        return host.to_string();
    }
    // Bare IPv6 address (contains multiple colons, no port): bracket + append :0.
    let colon_count = host.chars().filter(|&c| c == ':').count();
    if colon_count > 1 {
        return format!("[{}]:0", host);
    }
    // A host:port pair (exactly one colon): leave as-is.
    if colon_count == 1 {
        return host.to_string();
    }
    // Bare hostname or bare IPv4: append :0.
    format!("{}:0", host)
}

impl Interp {
    /// Module-level dispatch for `std/net` (general net utilities).
    pub(crate) async fn call_net(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            "lookup" => {
                let host = want_string(&arg(args, 0), span, "net.lookup")?;
                self.net_lookup(host.to_string(), span).await
            }
            "lookupOne" => {
                let host = want_string(&arg(args, 0), span, "net.lookupOne")?;
                let pair = self.net_lookup(host.to_string(), span).await?;
                // Unwrap the [arr, err] pair: if err != nil, pass it through.
                // If arr is empty, return an error.
                if let Value::Array(ref a) = pair {
                    let (val, err) = {
                        let b = a.borrow();
                        (b[0].clone(), b[1].clone())
                    };
                    if err != Value::Nil {
                        return Ok(pair);
                    }
                    // arr is the resolved list; get the first element.
                    if let Value::Array(ref list) = val {
                        let first = list.borrow().first().cloned();
                        return match first {
                            Some(ip) => Ok(make_pair(ip, Value::Nil)),
                            None => Ok(err_pair("net.lookupOne: no addresses returned".into())),
                        };
                    }
                }
                Ok(pair)
            }
            _ => Err(AsError::at(format!("std/net has no function '{}'", func), span).into()),
        }
    }

    /// Resolve `host` to a de-duplicated list of IP strings. Returns Tier-1
    /// `[array<string>, nil]` on success or `[nil, err]` on failure.
    async fn net_lookup(&self, host: String, span: Span) -> Result<Value, Control> {
        // FFI §4.4 stage-2 (net carve-out): re-check the resolved host against the
        // allow-list. Gate-12: when no `net` carve-out is configured this returns
        // immediately with no host comparison. The bare host (a single trailing
        // `:port` stripped — IPv6 literals carry multiple colons and are left whole)
        // is what an allow-list names.
        let bare = if host.starts_with('[') {
            host.split(']').next().unwrap_or(&host).trim_start_matches('[')
        } else if host.chars().filter(|&c| c == ':').count() == 1 {
            host.rsplit_once(':').map(|(h, _)| h).unwrap_or(&host)
        } else {
            &host
        };
        self.check_net_host(bare, span)?;
        let addr = to_addr(&host);
        let addrs = match tokio::net::lookup_host(&addr).await {
            Ok(a) => a,
            Err(e) => {
                return Ok(err_pair(format!("net.lookup \"{}\": {}", host, e)));
            }
        };
        // Collect de-duplicated IP strings, preserving first-seen order.
        let mut seen = std::collections::HashSet::new();
        let mut ips: Vec<Value> = Vec::new();
        for sa in addrs {
            let ip = sa.ip().to_string();
            if seen.insert(ip.clone()) {
                ips.push(Value::Str(crate::value::AStr::from(ip.as_str())));
            }
        }
        let arr = Value::Array(crate::value::ArrayCell::new(ips));
        Ok(make_pair(arr, Value::Nil))
    }
}

#[cfg(test)]
mod tests {
    /// Run an AScript program and return its captured output.
    async fn run(src: &str) -> String {
        crate::run_source(src).await.expect("program should run")
    }

    // ---- net.lookup ----

    #[tokio::test]
    async fn lookup_localhost_returns_non_empty_array() {
        // Resolving "localhost" must succeed and return at least one IP address
        // (127.0.0.1 on IPv4-capable resolvers, ::1 on IPv6-only, or both).
        let out = run(r#"
import { lookup } from "std/net"
let [ips, err] = await lookup("localhost")
print(err)
print(type(ips))
print(len(ips) >= 1)
"#)
        .await;
        assert_eq!(out, "nil\narray\ntrue\n");
    }

    #[tokio::test]
    async fn lookup_localhost_contains_known_ip() {
        // At least one of the returned addresses must be "127.0.0.1" or "::1".
        let out = run(r#"
import { lookup } from "std/net"
let [ips, err] = await lookup("localhost")
let found = false
for (ip in ips) {
    if (ip == "127.0.0.1" || ip == "::1") {
        found = true
    }
}
print(err)
print(found)
"#)
        .await;
        assert_eq!(out, "nil\ntrue\n");
    }

    #[tokio::test]
    async fn lookup_invalid_host_returns_err() {
        // A clearly invalid / NXDOMAIN host must produce [nil, err].
        let out = run(r#"
import { lookup } from "std/net"
let [ips, err] = await lookup("nonexistent.invalid.")
print(ips)
print(err != nil)
"#)
        .await;
        assert_eq!(out, "nil\ntrue\n");
    }

    // ---- net.lookupOne ----

    #[tokio::test]
    async fn lookup_one_localhost_returns_string() {
        // lookupOne must return a non-empty string IP for localhost.
        let out = run(r#"
import { lookupOne } from "std/net"
let [ip, err] = await lookupOne("localhost")
print(err)
print(type(ip))
print(len(ip) > 0)
"#)
        .await;
        assert_eq!(out, "nil\nstring\ntrue\n");
    }

    #[tokio::test]
    async fn lookup_one_invalid_host_returns_err() {
        let out = run(r#"
import { lookupOne } from "std/net"
let [ip, err] = await lookupOne("nonexistent.invalid.")
print(ip)
print(err != nil)
"#)
        .await;
        assert_eq!(out, "nil\ntrue\n");
    }
}
