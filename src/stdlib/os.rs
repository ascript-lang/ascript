//! `std/os` — host OS facts: pid, platform, arch, CPU count, hostname, temp dir.
//!
//! When the `sysinfo` feature is enabled this module also provides live system
//! metrics via the `sysinfo` crate:
//!   - `os.memory()` / `os.swap()` / `os.cpuUsage()` / `os.loadAvg()`
//!   - `os.disks()` / `os.uptime()` / `os.networkInterfaces()` / `os.localIp()`
//!
//! `cpuUsage` is async (two refreshes separated by `MINIMUM_CPU_UPDATE_INTERVAL`).
//! All other sysinfo functions are synchronous and are routed through the normal
//! `os::call` entry point. The async `cpuUsage` is handled in `Interp::call_os`
//! in `src/stdlib/mod.rs`.
//!
//! **Network interface IPs** come from `sysinfo::Networks` which in 0.31 provides
//! `NetworkData::ip_networks()` → `&[IpNetwork { addr: IpAddr, prefix: u8 }]`.
//! No additional crate is needed.

use super::bi;
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::Value;
// Container imports are only used by the sysinfo metric arms (object/array
// construction), so gate them to keep the `sysinfo`-off build warning-clean.
#[cfg(feature = "sysinfo")]
use indexmap::IndexMap;

pub fn exports() -> Vec<(&'static str, Value)> {
    // Base host facts; the `mut`-less binding keeps the `sysinfo`-off build
    // warning-clean (the metric entries below are only appended when the
    // feature is on).
    let v = vec![
        ("pid", bi("os.pid")),
        ("platform", bi("os.platform")),
        ("arch", bi("os.arch")),
        ("cpuCount", bi("os.cpuCount")),
        ("hostname", bi("os.hostname")),
        ("tempDir", bi("os.tempDir")),
    ];
    #[cfg(feature = "sysinfo")]
    let v = {
        let mut v = v;
        v.extend([
            ("memory", bi("os.memory")),
            ("swap", bi("os.swap")),
            ("cpuUsage", bi("os.cpuUsage")),
            ("loadAvg", bi("os.loadAvg")),
            ("disks", bi("os.disks")),
            ("uptime", bi("os.uptime")),
            ("networkInterfaces", bi("os.networkInterfaces")),
            ("localIp", bi("os.localIp")),
        ]);
        v
    };
    v
}

/// Helper: build a `Value::Object` from key-value pairs. Only used by the
/// sysinfo metric arms, so gated to keep the `sysinfo`-off build warning-clean.
#[cfg(feature = "sysinfo")]
fn make_obj(pairs: &[(&str, Value)]) -> Value {
    let mut map: IndexMap<String, Value> = IndexMap::new();
    for (k, v) in pairs {
        map.insert(k.to_string(), v.clone());
    }
    Value::Object(crate::value::ObjectCell::new(map))
}

pub fn call(func: &str, _args: &[Value], span: Span) -> Result<Value, Control> {
    match func {
        // pid() -> number — current process ID
        "pid" => Ok(Value::Float(std::process::id() as f64)),

        // platform() -> string — e.g. "macos", "linux", "windows"
        "platform" => Ok(Value::Str(std::env::consts::OS.into())),

        // arch() -> string — e.g. "aarch64", "x86_64"
        "arch" => Ok(Value::Str(std::env::consts::ARCH.into())),

        // cpuCount() -> number — available parallelism, fallback 1
        "cpuCount" => {
            let n = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1);
            Ok(Value::Float(n as f64))
        }

        // hostname() -> string — machine hostname, "unknown" on error
        "hostname" => {
            let name = hostname::get()
                .ok()
                .and_then(|o| o.into_string().ok())
                .unwrap_or_else(|| "unknown".into());
            Ok(Value::Str(name.into()))
        }

        // tempDir() -> string — OS temporary directory path
        "tempDir" => {
            let path = std::env::temp_dir().to_string_lossy().into_owned();
            Ok(Value::Str(path.into()))
        }

        // ---- sysinfo-backed synchronous metrics ----

        // memory() -> {total, used, free, available}  (bytes)
        #[cfg(feature = "sysinfo")]
        "memory" => {
            let mut sys = sysinfo::System::new();
            sys.refresh_memory();
            Ok(make_obj(&[
                ("total", Value::Float(sys.total_memory() as f64)),
                ("used", Value::Float(sys.used_memory() as f64)),
                ("free", Value::Float(sys.free_memory() as f64)),
                ("available", Value::Float(sys.available_memory() as f64)),
            ]))
        }

        // swap() -> {total, used, free}  (bytes)
        #[cfg(feature = "sysinfo")]
        "swap" => {
            let mut sys = sysinfo::System::new();
            sys.refresh_memory();
            Ok(make_obj(&[
                ("total", Value::Float(sys.total_swap() as f64)),
                ("used", Value::Float(sys.used_swap() as f64)),
                ("free", Value::Float(sys.free_swap() as f64)),
            ]))
        }

        // loadAvg() -> {one, five, fifteen}
        #[cfg(feature = "sysinfo")]
        "loadAvg" => {
            let la = sysinfo::System::load_average();
            Ok(make_obj(&[
                ("one", Value::Float(la.one)),
                ("five", Value::Float(la.five)),
                ("fifteen", Value::Float(la.fifteen)),
            ]))
        }

        // disks() -> array<{mount, total, free, available}>
        #[cfg(feature = "sysinfo")]
        "disks" => {
            let disks = sysinfo::Disks::new_with_refreshed_list();
            let entries: Vec<Value> = disks
                .list()
                .iter()
                .map(|d| {
                    // `free` and `available` are both available_space(): sysinfo
                    // 0.31's Disk has no separate free_space() accessor.
                    make_obj(&[
                        (
                            "mount",
                            Value::Str(d.mount_point().to_string_lossy().as_ref().into()),
                        ),
                        ("total", Value::Float(d.total_space() as f64)),
                        ("free", Value::Float(d.available_space() as f64)),
                        ("available", Value::Float(d.available_space() as f64)),
                    ])
                })
                .collect();
            Ok(Value::Array(crate::value::ArrayCell::new(entries)))
        }

        // uptime() -> number  (seconds)
        #[cfg(feature = "sysinfo")]
        "uptime" => Ok(Value::Float(sysinfo::System::uptime() as f64)),

        // networkInterfaces() -> array<{name, addresses: array<string>}>
        // Uses sysinfo::Networks which provides ip_networks() in 0.31.
        #[cfg(feature = "sysinfo")]
        "networkInterfaces" => {
            let networks = sysinfo::Networks::new_with_refreshed_list();
            let entries: Vec<Value> = networks
                .list()
                .iter()
                .map(|(name, data)| {
                    let addrs: Vec<Value> = data
                        .ip_networks()
                        .iter()
                        .map(|ip| Value::Str(ip.addr.to_string().into()))
                        .collect();
                    make_obj(&[
                        ("name", Value::Str(name.as_str().into())),
                        (
                            "addresses",
                            Value::Array(crate::value::ArrayCell::new(addrs)),
                        ),
                    ])
                })
                .collect();
            Ok(Value::Array(crate::value::ArrayCell::new(entries)))
        }

        // localIp() -> [string, err]  — best-effort primary non-loopback IPv4 (Tier-1)
        // Uses networkInterfaces() logic: first non-loopback IPv4 found across all
        // interfaces.
        #[cfg(feature = "sysinfo")]
        "localIp" => {
            use std::net::IpAddr;
            let networks = sysinfo::Networks::new_with_refreshed_list();
            let mut found: Option<String> = None;
            'outer: for data in networks.list().values() {
                for ip_net in data.ip_networks() {
                    if let IpAddr::V4(v4) = ip_net.addr {
                        if !v4.is_loopback() && !v4.is_link_local() && !v4.is_unspecified() {
                            found = Some(v4.to_string());
                            break 'outer;
                        }
                    }
                }
            }
            match found {
                Some(ip) => Ok(crate::interp::make_pair(Value::Str(ip.into()), Value::Nil)),
                None => Ok(crate::interp::make_pair(
                    Value::Nil,
                    crate::interp::make_error(Value::Str(
                        "os.localIp: no non-loopback IPv4 found".into(),
                    )),
                )),
            }
        }

        _ => Err(AsError::at(format!("std/os has no function '{}'", func), span).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::new(0, 0)
    }

    #[test]
    fn pid_is_positive_number() {
        let v = call("pid", &[], sp()).unwrap();
        match v {
            Value::Float(n) => assert!(n > 0.0, "pid should be > 0, got {}", n),
            other => panic!("pid() should return a Number, got {:?}", other),
        }
    }

    #[test]
    fn platform_is_nonempty_string() {
        let v = call("platform", &[], sp()).unwrap();
        match v {
            Value::Str(s) => assert!(!s.is_empty(), "platform should be non-empty"),
            other => panic!("platform() should return a Str, got {:?}", other),
        }
    }

    #[test]
    fn arch_is_nonempty_string() {
        let v = call("arch", &[], sp()).unwrap();
        match v {
            Value::Str(s) => assert!(!s.is_empty(), "arch should be non-empty"),
            other => panic!("arch() should return a Str, got {:?}", other),
        }
    }

    #[test]
    fn cpu_count_is_at_least_one() {
        let v = call("cpuCount", &[], sp()).unwrap();
        match v {
            Value::Float(n) => assert!(n >= 1.0, "cpuCount should be >= 1, got {}", n),
            other => panic!("cpuCount() should return a Number, got {:?}", other),
        }
    }

    #[test]
    fn hostname_is_nonempty_string() {
        let v = call("hostname", &[], sp()).unwrap();
        match v {
            Value::Str(s) => assert!(!s.is_empty(), "hostname should be non-empty"),
            other => panic!("hostname() should return a Str, got {:?}", other),
        }
    }

    #[test]
    fn temp_dir_is_nonempty_string() {
        let v = call("tempDir", &[], sp()).unwrap();
        match v {
            Value::Str(s) => assert!(!s.is_empty(), "tempDir should be non-empty"),
            other => panic!("tempDir() should return a Str, got {:?}", other),
        }
    }

    #[test]
    fn unknown_function_is_tier2_panic() {
        let err = call("noSuchFn", &[], sp());
        assert!(matches!(err, Err(Control::Panic(_))));
    }

    // ---- sysinfo-backed tests (feature = "sysinfo") ----

    #[cfg(feature = "sysinfo")]
    #[test]
    fn memory_returns_object_with_positive_total() {
        let v = call("memory", &[], sp()).unwrap();
        match v {
            Value::Object(o) => {
                let total = match o.get("total") {
                    Some(Value::Float(n)) => n,
                    other => panic!("memory().total should be a Number, got {:?}", other),
                };
                assert!(total > 0.0, "memory().total should be > 0, got {}", total);
                let used = match o.get("used") {
                    Some(Value::Float(n)) => n,
                    other => panic!("memory().used should be a Number, got {:?}", other),
                };
                assert!(
                    used <= total,
                    "memory().used ({}) should be <= total ({})",
                    used,
                    total
                );
                // Check all four keys exist
                assert!(o.contains_key("free"), "memory() missing 'free'");
                assert!(o.contains_key("available"), "memory() missing 'available'");
            }
            other => panic!("memory() should return an Object, got {:?}", other),
        }
    }

    #[cfg(feature = "sysinfo")]
    #[test]
    fn swap_returns_object_with_nonnegative_total() {
        let v = call("swap", &[], sp()).unwrap();
        match v {
            Value::Object(o) => {
                let total = match o.get("total") {
                    Some(Value::Float(n)) => n,
                    other => panic!("swap().total should be a Number, got {:?}", other),
                };
                assert!(total >= 0.0, "swap().total should be >= 0, got {}", total);
                assert!(o.contains_key("used"), "swap() missing 'used'");
                assert!(o.contains_key("free"), "swap() missing 'free'");
            }
            other => panic!("swap() should return an Object, got {:?}", other),
        }
    }

    #[cfg(feature = "sysinfo")]
    #[test]
    fn load_avg_returns_object_with_nonnegative_fields() {
        let v = call("loadAvg", &[], sp()).unwrap();
        match v {
            Value::Object(o) => {
                for key in &["one", "five", "fifteen"] {
                    match o.get(key) {
                        Some(Value::Float(n)) => {
                            assert!(n >= 0.0, "loadAvg().{} should be >= 0, got {}", key, n)
                        }
                        other => panic!("loadAvg().{} should be a Number, got {:?}", key, other),
                    }
                }
            }
            other => panic!("loadAvg() should return an Object, got {:?}", other),
        }
    }

    #[cfg(feature = "sysinfo")]
    #[test]
    fn disks_returns_array() {
        let v = call("disks", &[], sp()).unwrap();
        match v {
            Value::Array(arr) => {
                let items = arr.borrow();
                // Disks may be empty in a sandbox; if present, check fields.
                for item in items.iter() {
                    match item {
                        Value::Object(o) => {
                            assert!(o.contains_key("mount"), "disk entry missing 'mount'");
                            assert!(o.contains_key("total"), "disk entry missing 'total'");
                            assert!(o.contains_key("free"), "disk entry missing 'free'");
                            assert!(
                                o.contains_key("available"),
                                "disk entry missing 'available'"
                            );
                        }
                        other => panic!("disks() array entry should be Object, got {:?}", other),
                    }
                }
            }
            other => panic!("disks() should return an Array, got {:?}", other),
        }
    }

    #[cfg(feature = "sysinfo")]
    #[test]
    fn uptime_returns_positive_number() {
        let v = call("uptime", &[], sp()).unwrap();
        match v {
            Value::Float(n) => assert!(n > 0.0, "uptime() should be > 0, got {}", n),
            other => panic!("uptime() should return a Number, got {:?}", other),
        }
    }

    #[cfg(feature = "sysinfo")]
    #[test]
    fn network_interfaces_returns_array() {
        let v = call("networkInterfaces", &[], sp()).unwrap();
        match v {
            Value::Array(arr) => {
                let items = arr.borrow();
                // May be empty in sandboxed CI; if present, check structure.
                for item in items.iter() {
                    match item {
                        Value::Object(o) => {
                            assert!(o.contains_key("name"), "interface entry missing 'name'");
                            assert!(
                                o.contains_key("addresses"),
                                "interface entry missing 'addresses'"
                            );
                            match o.get("addresses") {
                                Some(Value::Array(_)) => {}
                                other => {
                                    panic!("interface.addresses should be Array, got {:?}", other)
                                }
                            }
                        }
                        other => panic!(
                            "networkInterfaces() entry should be Object, got {:?}",
                            other
                        ),
                    }
                }
            }
            other => panic!(
                "networkInterfaces() should return an Array, got {:?}",
                other
            ),
        }
    }

    #[cfg(feature = "sysinfo")]
    #[tokio::test]
    async fn cpu_usage_returns_percentage_in_range() {
        // cpuUsage is async (sleeps ~200ms); run via the interpreter.
        let out = crate::run_source(
            r#"
import { cpuUsage } from "std/os"
let pct = await cpuUsage()
print(type(pct))
print(pct >= 0)
print(pct <= 100)
"#,
        )
        .await
        .expect("cpuUsage program should run");
        // NUM §4: a CPU usage percentage is fractional → `float`.
        assert_eq!(out, "float\ntrue\ntrue\n", "cpuUsage output: {}", out);
    }

    #[cfg(feature = "sysinfo")]
    #[test]
    fn local_ip_returns_pair() {
        let v = call("localIp", &[], sp()).unwrap();
        match v {
            Value::Array(arr) => {
                let items = arr.borrow();
                assert_eq!(items.len(), 2, "localIp() should return a 2-element array");
                let (val, err) = (&items[0], &items[1]);
                // Either val is a non-empty string and err is nil, or val is nil and err is an object.
                match (val, err) {
                    (Value::Str(s), Value::Nil) => {
                        assert!(!s.is_empty(), "localIp() address should be non-empty");
                    }
                    (Value::Nil, Value::Object(_)) => {
                        // No non-loopback interface found — acceptable in a sandbox.
                    }
                    other => panic!("localIp() pair has unexpected shape: {:?}", other),
                }
            }
            other => panic!("localIp() should return an Array pair, got {:?}", other),
        }
    }
}
