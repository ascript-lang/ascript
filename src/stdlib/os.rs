//! `std/os` — host OS facts: pid, platform, arch, CPU count, hostname, temp dir.
//!
//! All functions are synchronous and take no arguments (or ignore them).

use super::bi;
use crate::error::AsError;
use crate::interp::Control;
use crate::span::Span;
use crate::value::Value;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("pid", bi("os.pid")),
        ("platform", bi("os.platform")),
        ("arch", bi("os.arch")),
        ("cpuCount", bi("os.cpuCount")),
        ("hostname", bi("os.hostname")),
        ("tempDir", bi("os.tempDir")),
    ]
}

pub fn call(func: &str, _args: &[Value], span: Span) -> Result<Value, Control> {
    match func {
        // pid() -> number — current process ID
        "pid" => Ok(Value::Number(std::process::id() as f64)),

        // platform() -> string — e.g. "macos", "linux", "windows"
        "platform" => Ok(Value::Str(std::env::consts::OS.into())),

        // arch() -> string — e.g. "aarch64", "x86_64"
        "arch" => Ok(Value::Str(std::env::consts::ARCH.into())),

        // cpuCount() -> number — available parallelism, fallback 1
        "cpuCount" => {
            let n = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1);
            Ok(Value::Number(n as f64))
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
            let path = std::env::temp_dir()
                .to_string_lossy()
                .into_owned();
            Ok(Value::Str(path.into()))
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
            Value::Number(n) => assert!(n > 0.0, "pid should be > 0, got {}", n),
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
            Value::Number(n) => assert!(n >= 1.0, "cpuCount should be >= 1, got {}", n),
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
}
