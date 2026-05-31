//! `std/crypto` — hashing (sha256/sha512/md5), HMAC, CSPRNG bytes,
//! password hashing (argon2 + bcrypt), and non-cryptographic checksums
//! (crc32/xxhash).
//!
//! Deterministic hashes return a plain lowercase-hex string. Password hashing
//! is fallible (RNG / encoding), so it follows the Tier-1 `[value, err]`
//! convention. Argument-type misuse is a Tier-2 panic (spec §11.3).

use super::{arg, bi, want_number, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use hmac::{Hmac, Mac};
use md5::Md5;
use rand::RngCore;
use sha2::{Digest, Sha256, Sha512};

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("sha256", bi("crypto.sha256")),
        ("sha512", bi("crypto.sha512")),
        ("md5", bi("crypto.md5")),
        ("hmacSha256", bi("crypto.hmacSha256")),
        ("randomBytes", bi("crypto.randomBytes")),
        ("hashPassword", bi("crypto.hashPassword")),
        ("verifyPassword", bi("crypto.verifyPassword")),
        ("bcryptHash", bi("crypto.bcryptHash")),
        ("bcryptVerify", bi("crypto.bcryptVerify")),
        ("crc32", bi("crypto.crc32")),
        ("xxhash", bi("crypto.xxhash")),
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

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("crypto.{}", f);
    match func {
        "sha256" => {
            let src = source_bytes(&arg(args, 0), span, &ctx("sha256"))?;
            let digest = Sha256::digest(&src);
            Ok(Value::Str(hex::encode(digest).into()))
        }
        "sha512" => {
            let src = source_bytes(&arg(args, 0), span, &ctx("sha512"))?;
            let digest = Sha512::digest(&src);
            Ok(Value::Str(hex::encode(digest).into()))
        }
        "md5" => {
            let src = source_bytes(&arg(args, 0), span, &ctx("md5"))?;
            let digest = Md5::digest(&src);
            Ok(Value::Str(hex::encode(digest).into()))
        }
        "hmacSha256" => {
            let key = source_bytes(&arg(args, 0), span, &ctx("hmacSha256"))?;
            let data = source_bytes(&arg(args, 1), span, &ctx("hmacSha256"))?;
            // `new_from_slice` accepts a key of any length (HMAC pads/hashes it),
            // so this never fails for SHA-256.
            let mut mac =
                Hmac::<Sha256>::new_from_slice(&key).expect("HMAC accepts any key length");
            mac.update(&data);
            let tag = mac.finalize().into_bytes();
            Ok(Value::Str(hex::encode(tag).into()))
        }
        "randomBytes" => {
            let n = want_number(&arg(args, 0), span, &ctx("randomBytes"))?;
            // Bound the length: a huge/fractional `n as usize` would saturate
            // into an alloc-abort rather than a clean Tier-2 panic. 16 MiB cap.
            const MAX_RANDOM_BYTES: f64 = 16_777_216.0;
            if !n.is_finite() || n < 0.0 || n.fract() != 0.0 || n > MAX_RANDOM_BYTES {
                return Err(AsError::at(
                    "crypto.randomBytes: length must be a non-negative integer <= 16777216",
                    span,
                )
                .into());
            }
            let n = n as usize;
            let mut buf = vec![0u8; n];
            rand::thread_rng().fill_bytes(&mut buf);
            Ok(bytes_val(buf))
        }
        "hashPassword" => {
            let pw = source_bytes(&arg(args, 0), span, &ctx("hashPassword"))?;
            let salt = SaltString::generate(&mut OsRng);
            match Argon2::default().hash_password(&pw, &salt) {
                Ok(hash) => Ok(make_pair(Value::Str(hash.to_string().into()), Value::Nil)),
                Err(e) => Ok(make_pair(
                    Value::Nil,
                    make_error(Value::Str(format!("argon2 hash failed: {}", e).into())),
                )),
            }
        }
        "verifyPassword" => {
            let pw = source_bytes(&arg(args, 0), span, &ctx("verifyPassword"))?;
            let phc = want_string(&arg(args, 1), span, &ctx("verifyPassword"))?;
            // A malformed PHC string or a non-match both verify as `false`.
            let ok = PasswordHash::new(&phc)
                .map(|parsed| Argon2::default().verify_password(&pw, &parsed).is_ok())
                .unwrap_or(false);
            Ok(Value::Bool(ok))
        }
        "bcryptHash" => {
            let pw = source_bytes(&arg(args, 0), span, &ctx("bcryptHash"))?;
            let cost = match args.get(1) {
                None | Some(Value::Nil) => bcrypt::DEFAULT_COST,
                Some(v) => {
                    let c = want_number(v, span, &ctx("bcryptHash"))?;
                    // bcrypt's valid cost range is 4..=31; reject anything else
                    // (incl. non-integers) as a Tier-2 panic.
                    if !c.is_finite() || c.fract() != 0.0 || !(4.0..=31.0).contains(&c) {
                        return Err(AsError::at(
                            "crypto.bcryptHash: cost must be an integer in 4..=31",
                            span,
                        )
                        .into());
                    }
                    c as u32
                }
            };
            match bcrypt::hash(&pw, cost) {
                Ok(h) => Ok(make_pair(Value::Str(h.into()), Value::Nil)),
                Err(e) => Ok(make_pair(
                    Value::Nil,
                    make_error(Value::Str(format!("bcrypt hash failed: {}", e).into())),
                )),
            }
        }
        "bcryptVerify" => {
            let pw = source_bytes(&arg(args, 0), span, &ctx("bcryptVerify"))?;
            let hash = want_string(&arg(args, 1), span, &ctx("bcryptVerify"))?;
            // A malformed hash or a non-match both verify as `false`.
            let ok = bcrypt::verify(&pw, &hash).unwrap_or(false);
            Ok(Value::Bool(ok))
        }
        "crc32" => {
            let bytes = source_bytes(&arg(args, 0), span, &ctx("crc32"))?;
            let mut h = crc32fast::Hasher::new();
            h.update(&bytes);
            Ok(Value::Number(h.finalize() as f64))
        }
        "xxhash" => {
            let bytes = source_bytes(&arg(args, 0), span, &ctx("xxhash"))?;
            let digest = xxhash_rust::xxh64::xxh64(&bytes, 0);
            Ok(Value::Str(format!("{:016x}", digest).into()))
        }
        _ => Err(AsError::at(format!("std/crypto has no function '{}'", func), span).into()),
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

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            call("sha256", &[s("")], sp()).unwrap(),
            s("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        assert_eq!(
            call("sha256", &[s("abc")], sp()).unwrap(),
            s("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    #[test]
    fn sha512_known_vector() {
        assert_eq!(
            call("sha512", &[s("abc")], sp()).unwrap(),
            s("ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f")
        );
    }

    #[test]
    fn md5_known_vectors() {
        assert_eq!(
            call("md5", &[s("abc")], sp()).unwrap(),
            s("900150983cd24fb0d6963f7d28e17f72")
        );
        assert_eq!(
            call("md5", &[s("")], sp()).unwrap(),
            s("d41d8cd98f00b204e9800998ecf8427e")
        );
    }

    #[test]
    fn hmac_sha256_known_vector() {
        assert_eq!(
            call(
                "hmacSha256",
                &[s("key"), s("The quick brown fox jumps over the lazy dog")],
                sp()
            )
            .unwrap(),
            s("f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8")
        );
    }

    #[test]
    fn random_bytes_len_and_distinct() {
        let a = call("randomBytes", &[Value::Number(16.0)], sp()).unwrap();
        let b = call("randomBytes", &[Value::Number(16.0)], sp()).unwrap();
        match (&a, &b) {
            (Value::Bytes(ba), Value::Bytes(bb)) => {
                assert_eq!(ba.borrow().len(), 16);
                assert_eq!(bb.borrow().len(), 16);
                // Two 16-byte CSPRNG draws are overwhelmingly unlikely to match.
                assert_ne!(*ba.borrow(), *bb.borrow());
            }
            _ => panic!("randomBytes should return bytes"),
        }
    }

    #[test]
    fn random_bytes_bounds_are_tier2() {
        // Negative length is rejected.
        assert!(call("randomBytes", &[Value::Number(-1.0)], sp()).is_err());
        // A huge length would saturate `as usize` into an alloc-abort; reject it.
        assert!(call("randomBytes", &[Value::Number(1e30)], sp()).is_err());
        // Fractional (non-integer) lengths are rejected.
        assert!(call("randomBytes", &[Value::Number(1.5)], sp()).is_err());
        // Non-finite lengths are rejected.
        assert!(call("randomBytes", &[Value::Number(f64::NAN)], sp()).is_err());
        // The cap itself is allowed; one past it is not.
        assert!(call("randomBytes", &[Value::Number(16_777_217.0)], sp()).is_err());
    }

    #[test]
    fn bcrypt_cost_bounds_are_tier2() {
        // bcrypt's valid cost range is 4..=31; out-of-range is a Tier-2 panic.
        assert!(call("bcryptHash", &[s("pw"), Value::Number(99.0)], sp()).is_err());
        assert!(call("bcryptHash", &[s("pw"), Value::Number(3.0)], sp()).is_err());
        assert!(call("bcryptHash", &[s("pw"), Value::Number(4.5)], sp()).is_err());
    }

    #[test]
    fn argon2_password_roundtrip() {
        let pair = call("hashPassword", &[s("secret")], sp()).unwrap();
        let (phc, err) = match &pair {
            Value::Array(a) => {
                let v = a.borrow();
                (v[0].clone(), v[1].clone())
            }
            _ => panic!("hashPassword should return a pair"),
        };
        assert_eq!(err, Value::Nil);
        let phc_str = match &phc {
            Value::Str(s) => s.to_string(),
            _ => panic!("phc should be a string"),
        };
        assert!(phc_str.starts_with("$argon2"));
        assert_eq!(
            call("verifyPassword", &[s("secret"), s(&phc_str)], sp()).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            call("verifyPassword", &[s("wrong"), s(&phc_str)], sp()).unwrap(),
            Value::Bool(false)
        );
    }

    #[test]
    fn verify_password_malformed_is_false() {
        assert_eq!(
            call("verifyPassword", &[s("secret"), s("not-a-phc-string")], sp()).unwrap(),
            Value::Bool(false)
        );
    }

    #[test]
    fn bcrypt_roundtrip() {
        let pair = call("bcryptHash", &[s("secret")], sp()).unwrap();
        let (hash, err) = match &pair {
            Value::Array(a) => {
                let v = a.borrow();
                (v[0].clone(), v[1].clone())
            }
            _ => panic!("bcryptHash should return a pair"),
        };
        assert_eq!(err, Value::Nil);
        let hash_str = match &hash {
            Value::Str(s) => s.to_string(),
            _ => panic!("bcrypt hash should be a string"),
        };
        assert_eq!(
            call("bcryptVerify", &[s("secret"), s(&hash_str)], sp()).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            call("bcryptVerify", &[s("wrong"), s(&hash_str)], sp()).unwrap(),
            Value::Bool(false)
        );
    }

    #[test]
    fn bcrypt_custom_cost() {
        let pair = call("bcryptHash", &[s("pw"), Value::Number(4.0)], sp()).unwrap();
        let hash_str = match &pair {
            Value::Array(a) => match &a.borrow()[0] {
                Value::Str(s) => s.to_string(),
                _ => panic!("expected string"),
            },
            _ => panic!("expected pair"),
        };
        // bcrypt encodes the cost as a two-digit field: `$2b$04$...`.
        assert!(hash_str.contains("$04$"), "got {}", hash_str);
    }

    #[test]
    fn checksums() {
        // crc32 of "hello" (IEEE) — crc32fast uses the standard IEEE polynomial
        let result = call("crc32", &[s("hello")], sp()).unwrap();
        // We'll verify it's the correct CRC-32 value; the exact constant will be
        // confirmed by the implementation (907060870 = 0x3610A686).
        assert_eq!(result, Value::Number(907060870.0));
        // xxhash returns a 16-char lowercase hex string (xxh64)
        let r = call("xxhash", &[s("hello")], sp()).unwrap();
        if let Value::Str(ref hex_str) = r {
            assert_eq!(hex_str.len(), 16);
            assert!(hex_str.chars().all(|c| c.is_ascii_hexdigit()));
        } else {
            panic!("xxhash should return a string, got {:?}", r);
        }
        // Pinned known vector: canonical xxh64("hello", seed=0) = 0x26c7827d889f6da3.
        assert_eq!(
            call("xxhash", &[s("hello")], sp()).unwrap(),
            Value::Str("26c7827d889f6da3".into())
        );
        // bytes input also works for crc32
        assert_eq!(
            call(
                "crc32",
                &[Value::Bytes(Rc::new(RefCell::new(b"hello".to_vec())))],
                sp()
            )
            .unwrap(),
            Value::Number(907060870.0)
        );
        // bytes input also works for xxhash
        let r2 = call(
            "xxhash",
            &[Value::Bytes(Rc::new(RefCell::new(b"hello".to_vec())))],
            sp(),
        )
        .unwrap();
        // Should produce same result as string "hello"
        assert_eq!(r, r2);
    }

    #[test]
    fn arg_type_misuse_is_tier2_panic() {
        // A number is not a valid data source → Tier-2 (Control error).
        assert!(call("sha256", &[Value::Number(42.0)], sp()).is_err());
        assert!(call("md5", &[Value::Bool(true)], sp()).is_err());
        // hmac with a non-string/bytes key.
        assert!(call("hmacSha256", &[Value::Number(1.0), s("x")], sp()).is_err());
        // The checksum arms reject non-string/bytes input too.
        assert!(call("crc32", &[Value::Number(1.0)], sp()).is_err());
        assert!(call("xxhash", &[Value::Number(1.0)], sp()).is_err());
    }
}
