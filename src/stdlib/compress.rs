//! `std/compress` — gzip/deflate (de)compression and in-memory zip archives.
//!
//! Compression functions accept a string (encoded as UTF-8) or bytes and return
//! bytes. Decompression takes bytes and is fallible, so it follows the Tier-1
//! `[value, err]` convention; likewise `zipExtract`. `zipCreate` is Tier-1
//! (a bad entry shape / I/O failure). Argument-type misuse is a Tier-2 panic
//! (spec §11.3).

use super::{arg, bi, want_array, want_bytes};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

use std::io::{Read, Write};

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("gzip", bi("compress.gzip")),
        ("gunzip", bi("compress.gunzip")),
        ("deflate", bi("compress.deflate")),
        ("inflate", bi("compress.inflate")),
        ("zipCreate", bi("compress.zipCreate")),
        ("zipExtract", bi("compress.zipExtract")),
    ]
}

fn bytes_val(v: Vec<u8>) -> Value {
    Value::Bytes(Rc::new(RefCell::new(v)))
}

/// Accept bytes OR a string (encoded as UTF-8) as a source of raw bytes.
fn source_bytes(v: &Value, span: Span, ctx: &str) -> Result<Vec<u8>, Control> {
    match v {
        Value::Bytes(b) => Ok(b.borrow().clone()),
        Value::Str(s) => Ok(s.as_bytes().to_vec()),
        _ => Err(AsError::at(
            format!(
                "{} expects bytes or a string, got {}",
                ctx,
                crate::interp::type_name(v)
            ),
            span,
        )
        .into()),
    }
}

fn err_pair(msg: String) -> Value {
    make_pair(Value::Nil, make_error(Value::Str(msg.into())))
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("compress.{}", f);
    match func {
        "gzip" => {
            let src = source_bytes(&arg(args, 0), span, &ctx("gzip"))?;
            let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(&src)
                .and_then(|_| enc.finish())
                .map(bytes_val)
                .map_err(|e| AsError::at(format!("gzip failed: {}", e), span).into())
        }
        "gunzip" => {
            let src = want_bytes(&arg(args, 0), span, &ctx("gunzip"))?;
            let raw = src.borrow().clone();
            let mut dec = flate2::read::GzDecoder::new(&raw[..]);
            let mut out = Vec::new();
            match dec.read_to_end(&mut out) {
                Ok(_) => Ok(make_pair(bytes_val(out), Value::Nil)),
                Err(e) => Ok(err_pair(format!("gunzip failed: {}", e))),
            }
        }
        "deflate" => {
            let src = source_bytes(&arg(args, 0), span, &ctx("deflate"))?;
            let mut enc =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(&src)
                .and_then(|_| enc.finish())
                .map(bytes_val)
                .map_err(|e| AsError::at(format!("deflate failed: {}", e), span).into())
        }
        "inflate" => {
            let src = want_bytes(&arg(args, 0), span, &ctx("inflate"))?;
            let raw = src.borrow().clone();
            let mut dec = flate2::read::DeflateDecoder::new(&raw[..]);
            let mut out = Vec::new();
            match dec.read_to_end(&mut out) {
                Ok(_) => Ok(make_pair(bytes_val(out), Value::Nil)),
                Err(e) => Ok(err_pair(format!("inflate failed: {}", e))),
            }
        }
        "zipCreate" => {
            let entries = want_array(&arg(args, 0), span, &ctx("zipCreate"))?;
            let entries = entries.borrow().clone();
            match build_zip(&entries, span) {
                Ok(bytes) => Ok(make_pair(bytes_val(bytes), Value::Nil)),
                Err(ZipBuildError::Type(c)) => Err(c),
                Err(ZipBuildError::Tier1(msg)) => Ok(err_pair(msg)),
            }
        }
        "zipExtract" => {
            let src = want_bytes(&arg(args, 0), span, &ctx("zipExtract"))?;
            let raw = src.borrow().clone();
            match extract_zip(&raw) {
                Ok(arr) => Ok(make_pair(
                    Value::Array(gcmodule::Cc::new(RefCell::new(arr))),
                    Value::Nil,
                )),
                Err(msg) => Ok(err_pair(msg)),
            }
        }
        _ => Err(AsError::at(format!("std/compress has no function '{}'", func), span).into()),
    }
}

/// `zipCreate` can fail either with a Tier-2 type error (a malformed entry) or a
/// Tier-1 archive/I-O error.
enum ZipBuildError {
    Type(Control),
    Tier1(String),
}

fn build_zip(entries: &[Value], span: Span) -> Result<Vec<u8>, ZipBuildError> {
    use zip::write::{SimpleFileOptions, ZipWriter};
    let cursor = std::io::Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for entry in entries {
        let obj = match entry {
            Value::Object(o) => o.clone(),
            _ => {
                return Err(ZipBuildError::Type(
                    AsError::at(
                        format!(
                            "compress.zipCreate entry must be an object, got {}",
                            crate::interp::type_name(entry)
                        ),
                        span,
                    )
                    .into(),
                ))
            }
        };
        let obj = obj.borrow();
        let name = match obj.get("name") {
            Some(Value::Str(s)) => s.to_string(),
            other => {
                return Err(ZipBuildError::Type(
                    AsError::at(
                        format!(
                            "compress.zipCreate entry.name must be a string, got {}",
                            crate::interp::type_name(other.unwrap_or(&Value::Nil))
                        ),
                        span,
                    )
                    .into(),
                ))
            }
        };
        let data = match obj.get("data") {
            Some(Value::Bytes(b)) => b.borrow().clone(),
            Some(Value::Str(s)) => s.as_bytes().to_vec(),
            other => {
                return Err(ZipBuildError::Type(
                    AsError::at(
                        format!(
                            "compress.zipCreate entry.data must be bytes or a string, got {}",
                            crate::interp::type_name(other.unwrap_or(&Value::Nil))
                        ),
                        span,
                    )
                    .into(),
                ))
            }
        };
        writer
            .start_file(name, opts)
            .map_err(|e| ZipBuildError::Tier1(format!("zipCreate failed: {}", e)))?;
        writer
            .write_all(&data)
            .map_err(|e| ZipBuildError::Tier1(format!("zipCreate failed: {}", e)))?;
    }
    let cursor = writer
        .finish()
        .map_err(|e| ZipBuildError::Tier1(format!("zipCreate failed: {}", e)))?;
    Ok(cursor.into_inner())
}

fn extract_zip(raw: &[u8]) -> Result<Vec<Value>, String> {
    let cursor = std::io::Cursor::new(raw);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| format!("zipExtract failed: {}", e))?;
    let mut out = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("zipExtract failed: {}", e))?;
        let name = file.name().to_string();
        let mut data = Vec::new();
        // Directory entries have no contents; read_to_end on them is fine (yields 0).
        file.read_to_end(&mut data)
            .map_err(|e| format!("zipExtract failed: {}", e))?;
        let mut obj = indexmap::IndexMap::new();
        obj.insert("name".to_string(), Value::Str(name.into()));
        obj.insert("data".to_string(), bytes_val(data));
        out.push(Value::Object(crate::value::ObjectCell::new(obj)));
    }
    Ok(out)
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
    fn b(v: Vec<u8>) -> Value {
        Value::Bytes(Rc::new(RefCell::new(v)))
    }
    fn as_bytes(v: &Value) -> Vec<u8> {
        match v {
            Value::Bytes(b) => b.borrow().clone(),
            _ => panic!("expected bytes, got {:?}", v),
        }
    }
    /// Pull `[value, nil]` apart, asserting the err slot is nil.
    fn ok_value(v: Value) -> Value {
        match v {
            Value::Array(a) => {
                let a = a.borrow();
                assert_eq!(a.len(), 2, "expected a [value, err] pair");
                assert_eq!(a[1], Value::Nil, "expected nil err, got {:?}", a[1]);
                a[0].clone()
            }
            _ => panic!("expected a pair, got {:?}", v),
        }
    }
    fn is_tier1_err(v: &Value) -> bool {
        match v {
            Value::Array(a) => {
                let a = a.borrow();
                a.len() == 2 && a[0] == Value::Nil && a[1] != Value::Nil
            }
            _ => false,
        }
    }

    #[test]
    fn gzip_gunzip_roundtrip_binary() {
        let original = vec![0x00u8, 0x01, 0xFF, 0x7F, 0x80, 0x00, 0xFF];
        let gz = call("gzip", &[b(original.clone())], sp()).unwrap();
        let back = ok_value(call("gunzip", std::slice::from_ref(&gz), sp()).unwrap());
        assert_eq!(as_bytes(&back), original);
    }

    #[test]
    fn gzip_actually_compresses_repetitive() {
        let original = "ABCD".repeat(1000); // 4000 bytes, highly repetitive
        let gz = call("gzip", &[s(&original)], sp()).unwrap();
        let gz_len = as_bytes(&gz).len();
        assert!(
            gz_len < original.len(),
            "gzip should shrink repetitive input: {} >= {}",
            gz_len,
            original.len()
        );
        let back = ok_value(call("gunzip", std::slice::from_ref(&gz), sp()).unwrap());
        assert_eq!(as_bytes(&back), original.as_bytes());
    }

    #[test]
    fn deflate_inflate_roundtrip() {
        let original = "the quick brown fox jumps over the lazy dog".repeat(20);
        let df = call("deflate", &[s(&original)], sp()).unwrap();
        assert!(as_bytes(&df).len() < original.len());
        let back = ok_value(call("inflate", std::slice::from_ref(&df), sp()).unwrap());
        assert_eq!(as_bytes(&back), original.as_bytes());
    }

    #[test]
    fn gunzip_garbage_is_tier1_err() {
        let garbage = b(vec![1, 2, 3, 4, 5, 6, 7, 8]);
        let r = call("gunzip", &[garbage], sp()).unwrap();
        assert!(is_tier1_err(&r), "expected Tier-1 err, got {:?}", r);
    }

    #[test]
    fn inflate_garbage_is_tier1_err() {
        let garbage = b(vec![0xFF, 0xFF, 0xFF, 0xFF]);
        let r = call("inflate", &[garbage], sp()).unwrap();
        assert!(is_tier1_err(&r), "expected Tier-1 err, got {:?}", r);
    }

    #[test]
    fn zip_create_extract_roundtrip() {
        let mut e1 = indexmap::IndexMap::new();
        e1.insert("name".to_string(), s("a.txt"));
        e1.insert("data".to_string(), s("hello"));
        let mut e2 = indexmap::IndexMap::new();
        e2.insert("name".to_string(), s("b.bin"));
        e2.insert("data".to_string(), b(vec![0x00, 0xFF, 0x10, 0x42]));
        let entries = Value::Array(gcmodule::Cc::new(RefCell::new(vec![
            Value::Object(crate::value::ObjectCell::new(e1)),
            Value::Object(crate::value::ObjectCell::new(e2)),
        ])));

        let zipped = ok_value(call("zipCreate", &[entries], sp()).unwrap());
        let extracted = ok_value(call("zipExtract", &[zipped], sp()).unwrap());

        let arr = match &extracted {
            Value::Array(a) => a.borrow(),
            _ => panic!("expected array"),
        };
        assert_eq!(arr.len(), 2);
        let get = |v: &Value, k: &str| -> Value {
            match v {
                Value::Object(o) => o.borrow().get(k).cloned().unwrap(),
                _ => panic!("expected object"),
            }
        };
        assert_eq!(get(&arr[0], "name"), s("a.txt"));
        assert_eq!(as_bytes(&get(&arr[0], "data")), b"hello".to_vec());
        assert_eq!(get(&arr[1], "name"), s("b.bin"));
        assert_eq!(
            as_bytes(&get(&arr[1], "data")),
            vec![0x00, 0xFF, 0x10, 0x42]
        );
    }

    #[test]
    fn zip_extract_garbage_is_tier1_err() {
        let garbage = b(vec![1, 2, 3, 4]);
        let r = call("zipExtract", &[garbage], sp()).unwrap();
        assert!(is_tier1_err(&r), "expected Tier-1 err, got {:?}", r);
    }

    #[test]
    #[should_panic(expected = "expects bytes")]
    fn gunzip_string_is_tier2_panic() {
        // gunzip requires bytes; a string is an arg-type misuse → Tier-2.
        call("gunzip", &[s("notbytes")], sp()).unwrap();
    }

    #[test]
    #[should_panic(expected = "expects bytes or a string")]
    fn gzip_number_is_tier2_panic() {
        call("gzip", &[Value::Number(42.0)], sp()).unwrap();
    }

    #[tokio::test]
    async fn interp_gzip_roundtrip_e2e() {
        let src = r#"
import { gzip, gunzip } from "std/compress"
import { utf8Decode } from "std/encoding"
let compressed = gzip("hello compress world")
let [raw, err] = gunzip(compressed)
print(err)
let [text, derr] = utf8Decode(raw)
print(text)
"#;
        let out = crate::run_source(src).await.expect("program should run");
        assert_eq!(out, "nil\nhello compress world\n");
    }
}
