//! `std/env` — process environment access: read/set/unset variables, snapshot
//! all variables, and load a `.env` file (dotenvy).
//!
//! NOTE: `set`/`unset` and `loadDotenv` mutate the *process-global* environment.
//! AScript runs single-threaded, so this is safe, but the change is visible to
//! every subsequent `get`/`vars`/`std/process` spawn in the same process.

use super::{arg, bi, want_string};
use crate::error::AsError;
use crate::interp::{make_error, make_pair, Control};
use crate::span::Span;
use crate::value::Value;
use indexmap::IndexMap;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("get", bi("env.get")),
        ("set", bi("env.set")),
        ("unset", bi("env.unset")),
        ("vars", bi("env.vars")),
        ("loadDotenv", bi("env.loadDotenv")),
        // args() is routed through the Interp (needs cli_args access), but the
        // binding must appear here so `import { args } from "std/env"` resolves.
        ("args", bi("env.args")),
    ]
}

pub fn call(func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
    let ctx = |f: &str| format!("env.{}", f);
    match func {
        // get(name) -> string | nil
        "get" => {
            let name = want_string(&arg(args, 0), span, &ctx("get"))?;
            match std::env::var(name.as_ref()) {
                Ok(v) => Ok(Value::Str(v.into())),
                Err(_) => Ok(Value::Nil),
            }
        }
        // set(name, value) -> nil. Mutates the process-global environment.
        "set" => {
            let name = want_string(&arg(args, 0), span, &ctx("set"))?;
            let value = want_string(&arg(args, 1), span, &ctx("set"))?;
            std::env::set_var(name.as_ref(), value.as_ref());
            Ok(Value::Nil)
        }
        // unset(name) -> nil. Mutates the process-global environment.
        "unset" => {
            let name = want_string(&arg(args, 0), span, &ctx("unset"))?;
            std::env::remove_var(name.as_ref());
            Ok(Value::Nil)
        }
        // vars() -> object of all current environment variables (order arbitrary).
        "vars" => {
            let mut m = IndexMap::new();
            for (k, v) in std::env::vars() {
                m.insert(k, Value::Str(v.into()));
            }
            Ok(Value::Object(crate::value::ObjectCell::new(m)))
        }
        // loadDotenv(path?) -> [count, err]. Loads a `.env` file (default `.env`)
        // into the process env and returns the number of variables loaded.
        "loadDotenv" => {
            let path = match args.first() {
                None | Some(Value::Nil) => std::path::PathBuf::from(".env"),
                Some(_) => std::path::PathBuf::from(
                    want_string(&arg(args, 0), span, &ctx("loadDotenv"))?.as_ref(),
                ),
            };
            // Iterate the file's entries, setting each into the process env and
            // counting successfully-set vars. A read/parse failure → Tier-1 err.
            let iter = match dotenvy::from_path_iter(&path) {
                Ok(iter) => iter,
                Err(e) => {
                    return Ok(make_pair(
                        Value::Nil,
                        make_error(Value::Str(format!("cannot load dotenv: {}", e).into())),
                    ))
                }
            };
            let mut count = 0u64;
            for entry in iter {
                match entry {
                    Ok((k, v)) => {
                        std::env::set_var(&k, &v);
                        count += 1;
                    }
                    Err(e) => {
                        return Ok(make_pair(
                            Value::Nil,
                            make_error(Value::Str(format!("cannot parse dotenv: {}", e).into())),
                        ))
                    }
                }
            }
            Ok(make_pair(Value::Number(count as f64), Value::Nil))
        }
        _ => Err(AsError::at(format!("std/env has no function '{}'", func), span).into()),
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
    fn set_get_unset_roundtrip() {
        let key = "ASCRIPT_TEST_ENV_RT_8f31";
        // initially unset
        assert_eq!(call("get", &[s(key)], sp()).unwrap(), Value::Nil);
        // set then get
        assert_eq!(
            call("set", &[s(key), s("hello")], sp()).unwrap(),
            Value::Nil
        );
        assert_eq!(call("get", &[s(key)], sp()).unwrap(), s("hello"));
        // unset then get is nil again
        assert_eq!(call("unset", &[s(key)], sp()).unwrap(), Value::Nil);
        assert_eq!(call("get", &[s(key)], sp()).unwrap(), Value::Nil);
    }

    #[test]
    fn vars_contains_just_set_key() {
        let key = "ASCRIPT_TEST_ENV_VARS_a72c";
        call("set", &[s(key), s("present")], sp()).unwrap();
        let vars = call("vars", &[], sp()).unwrap();
        let obj = match &vars {
            Value::Object(o) => o.clone(),
            other => panic!("vars() should return an object, got {:?}", other),
        };
        assert_eq!(obj.borrow().get(key), Some(&s("present")));
        call("unset", &[s(key)], sp()).unwrap();
    }

    #[test]
    fn get_non_string_arg_is_tier2_panic() {
        let err = call("get", &[Value::Number(42.0)], sp());
        assert!(matches!(err, Err(Control::Panic(_))));
    }

    #[test]
    fn load_dotenv_loads_and_counts() {
        // Write a temp .env file with a unique key, load it, confirm count + value.
        let dir = std::env::temp_dir().join("ascript_env_test_dot_3b9e");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".env");
        std::fs::write(
            &path,
            "ASCRIPT_DOTENV_KEY_3b9e=from_dotenv\nASCRIPT_DOTENV_OTHER_3b9e=second\n",
        )
        .unwrap();

        let result = call("loadDotenv", &[s(path.to_str().unwrap())], sp()).unwrap();
        // result is [count, nil]
        let arr = match &result {
            Value::Array(a) => a.clone(),
            other => panic!("loadDotenv should return a pair, got {:?}", other),
        };
        let arr = arr.borrow();
        assert_eq!(arr[0], Value::Number(2.0));
        assert_eq!(arr[1], Value::Nil);
        // and the vars are now in the process env
        assert_eq!(
            call("get", &[s("ASCRIPT_DOTENV_KEY_3b9e")], sp()).unwrap(),
            s("from_dotenv")
        );

        std::fs::remove_dir_all(&dir).ok();
        call("unset", &[s("ASCRIPT_DOTENV_KEY_3b9e")], sp()).unwrap();
        call("unset", &[s("ASCRIPT_DOTENV_OTHER_3b9e")], sp()).unwrap();
    }

    #[test]
    fn load_dotenv_missing_file_is_tier1_err() {
        let missing = std::env::temp_dir().join("ascript_no_such_dir_zz/.env_nope_1k2j");
        let result = call("loadDotenv", &[s(missing.to_str().unwrap())], sp()).unwrap();
        // [nil, err]
        assert!(result.to_string().starts_with("[nil, {message:"));
    }
}
