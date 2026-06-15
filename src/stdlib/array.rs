//! `std/array` — array operations. Callback-taking functions (`map`, `filter`,
//! `reduce`, `sort`) live on `impl Interp` because they invoke user functions.

use super::{arg, bi, clamp_index, want_array, want_number};
use crate::error::AsError;
use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::{MapKey, Value, ValueKind};
use indexmap::IndexMap;

pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("map", bi("array.map")),
        ("filter", bi("array.filter")),
        ("reduce", bi("array.reduce")),
        ("push", bi("array.push")),
        ("pop", bi("array.pop")),
        ("slice", bi("array.slice")),
        ("sort", bi("array.sort")),
        ("contains", bi("array.contains")),
        ("get", bi("array.get")),
        ("find", bi("array.find")),
        ("findIndex", bi("array.findIndex")),
        ("some", bi("array.some")),
        ("every", bi("array.every")),
        ("indexOf", bi("array.indexOf")),
        ("flat", bi("array.flat")),
        ("flatMap", bi("array.flatMap")),
        ("reverse", bi("array.reverse")),
        ("concat", bi("array.concat")),
        ("first", bi("array.first")),
        ("last", bi("array.last")),
        ("unique", bi("array.unique")),
        ("take", bi("array.take")),
        ("drop", bi("array.drop")),
        ("chunk", bi("array.chunk")),
        ("zip", bi("array.zip")),
        ("groupBy", bi("array.groupBy")),
        ("partition", bi("array.partition")),
    ]
}

impl Interp {
    pub(crate) async fn call_array(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        let ctx = |f: &str| format!("array.{}", f);
        match func {
            "map" => {
                let arr = want_array(&arg(args, 0), span, &ctx("map"))?;
                let f = arg(args, 1);
                let items = arr.borrow().clone();
                let mut cb = self.callback_driver(f, span);
                let mut out = Vec::with_capacity(items.len());
                for item in items.into_iter() {
                    let v = cb.call1(item).await?;
                    out.push(v);
                }
                Ok(Value::array(out))
            }
            "filter" => {
                let arr = want_array(&arg(args, 0), span, &ctx("filter"))?;
                let f = arg(args, 1);
                let items = arr.borrow().clone();
                let mut cb = self.callback_driver(f, span);
                let mut out = Vec::new();
                for item in items.into_iter() {
                    let keep = cb.call1(item.clone()).await?;
                    if keep.is_truthy() {
                        out.push(item);
                    }
                }
                Ok(Value::array(out))
            }
            "reduce" => {
                let arr = want_array(&arg(args, 0), span, &ctx("reduce"))?;
                let f = arg(args, 1);
                let mut acc = arg(args, 2);
                let items = arr.borrow().clone();
                let mut cb = self.callback_driver(f, span);
                for item in items.into_iter() {
                    acc = cb.call2(acc, item).await?;
                }
                Ok(acc)
            }
            "push" => {
                let arr = want_array(&arg(args, 0), span, &ctx("push"))?;
                crate::interp::check_not_frozen(&arg(args, 0), span)?;
                let item = arg(args, 1);
                let mut b = arr.borrow_mut();
                b.push(item);
                // NUM §4: the new length is an `Int`.
                Ok(Value::int(b.len() as i64))
            }
            "pop" => {
                let arr = want_array(&arg(args, 0), span, &ctx("pop"))?;
                crate::interp::check_not_frozen(&arg(args, 0), span)?;
                let popped = arr.borrow_mut().pop();
                Ok(popped.unwrap_or(Value::nil()))
            }
            "slice" => {
                let arr = want_array(&arg(args, 0), span, &ctx("slice"))?;
                let b = arr.borrow();
                let len = b.len();
                let start = clamp_index(want_number(&arg(args, 1), span, &ctx("slice"))?, len);
                let end = match args.get(2).map(|v| v.kind()) {
                    None | Some(ValueKind::Nil) => len,
                    Some(_) => clamp_index(want_number(&args[2], span, &ctx("slice"))?, len),
                };
                let out: Vec<Value> = if start < end {
                    b[start..end].to_vec()
                } else {
                    Vec::new()
                };
                Ok(Value::array(out))
            }
            "contains" => {
                let arr = want_array(&arg(args, 0), span, &ctx("contains"))?;
                let needle = arg(args, 1);
                let found = arr.borrow().contains(&needle);
                Ok(Value::bool_(found))
            }
            "get" => {
                let arr = want_array(&arg(args, 0), span, &ctx("get"))?;
                let i = want_number(&arg(args, 1), span, &ctx("get"))?;
                if i < 0.0 || i.fract() != 0.0 {
                    return Ok(Value::nil());
                }
                let got = arr.borrow().get(i as usize).cloned();
                Ok(got.unwrap_or(Value::nil()))
            }
            "sort" => {
                let arr = want_array(&arg(args, 0), span, &ctx("sort"))?;
                let mut items = arr.borrow().clone();
                let cmp = args.get(1).cloned();
                let cmp_is_some = cmp
                    .as_ref()
                    .map(|v| !matches!(v.kind(), ValueKind::Nil))
                    .unwrap_or(false);
                if !cmp_is_some {
                    sort_default(&mut items, span)?;
                } else {
                    let f = cmp.unwrap();
                    // Insertion sort driven by the async comparator: insert each
                    // item before the first existing element it compares < 0 against.
                    // O(n^2), acceptable because each comparison is an async user-
                    // callback call (a standard borrow-free sort is awkward with an
                    // async comparator). Stable: insertion stops at the first strictly
                    // negative compare, so equal-key elements keep their input order.
                    let mut sorted: Vec<Value> = Vec::with_capacity(items.len());
                    let mut cb = self.callback_driver(f, span);
                    for item in items.into_iter() {
                        let mut lo = 0usize;
                        while lo < sorted.len() {
                            let r = cb.call2(item.clone(), sorted[lo].clone()).await?;
                            let n = match r.as_f64() {
                                Some(n) => n,
                                None => {
                                    return Err(AsError::at(
                                        format!(
                                            "array.sort comparator must return a number, got {}",
                                            crate::interp::type_name(&r)
                                        ),
                                        span,
                                    )
                                    .into())
                                }
                            };
                            if n < 0.0 {
                                break;
                            }
                            lo += 1;
                        }
                        sorted.insert(lo, item);
                    }
                    items = sorted;
                }
                Ok(Value::array(items))
            }
            "find" => {
                let a = want_array(&arg(args, 0), span, &ctx("find"))?;
                let f = arg(args, 1);
                let items = a.borrow().clone();
                let mut cb = self.callback_driver(f, span);
                for item in items.into_iter() {
                    if cb.call1(item.clone()).await?.is_truthy() {
                        return Ok(item);
                    }
                }
                Ok(Value::nil())
            }
            "findIndex" => {
                let a = want_array(&arg(args, 0), span, &ctx("findIndex"))?;
                let f = arg(args, 1);
                let items = a.borrow().clone();
                let mut cb = self.callback_driver(f, span);
                for (i, item) in items.into_iter().enumerate() {
                    if cb.call1(item).await?.is_truthy() {
                        // NUM §4: an index is an `Int`.
                        return Ok(Value::int(i as i64));
                    }
                }
                Ok(Value::int(-1))
            }
            "some" => {
                let a = want_array(&arg(args, 0), span, &ctx("some"))?;
                let f = arg(args, 1);
                let items = a.borrow().clone();
                let mut cb = self.callback_driver(f, span);
                for item in items.into_iter() {
                    if cb.call1(item).await?.is_truthy() {
                        return Ok(Value::bool_(true));
                    }
                }
                Ok(Value::bool_(false))
            }
            "every" => {
                let a = want_array(&arg(args, 0), span, &ctx("every"))?;
                let f = arg(args, 1);
                let items = a.borrow().clone();
                let mut cb = self.callback_driver(f, span);
                for item in items.into_iter() {
                    if !cb.call1(item).await?.is_truthy() {
                        return Ok(Value::bool_(false));
                    }
                }
                Ok(Value::bool_(true))
            }
            "indexOf" => {
                let a = want_array(&arg(args, 0), span, &ctx("indexOf"))?;
                let needle = arg(args, 1);
                let idx = a.borrow().iter().position(|x| *x == needle);
                // NUM §4: an index is an `Int`.
                Ok(Value::int(idx.map(|i| i as i64).unwrap_or(-1)))
            }
            "flat" => {
                let a = want_array(&arg(args, 0), span, &ctx("flat"))?;
                let depth = match args.get(1).map(|v| v.kind()) {
                    None | Some(ValueKind::Nil) => 1usize,
                    Some(_) => {
                        let d = want_number(&args[1], span, &ctx("flat"))?;
                        if d < 0.0 || d.fract() != 0.0 {
                            return Err(AsError::at(
                                "array.flat depth must be a non-negative integer",
                                span,
                            )
                            .into());
                        }
                        d as usize
                    }
                };
                let mut out = Vec::new();
                flatten_into(&a.borrow(), depth, &mut out);
                Ok(Value::array(out))
            }
            "flatMap" => {
                let a = want_array(&arg(args, 0), span, &ctx("flatMap"))?;
                let f = arg(args, 1);
                let items = a.borrow().clone();
                let mut cb = self.callback_driver(f, span);
                let mut out = Vec::new();
                for item in items.into_iter() {
                    let mapped = cb.call1(item).await?;
                    if let ValueKind::Array(inner) = mapped.kind() {
                        out.extend(inner.borrow().iter().cloned());
                    } else {
                        out.push(mapped);
                    }
                }
                Ok(Value::array(out))
            }
            "reverse" => {
                let a = want_array(&arg(args, 0), span, &ctx("reverse"))?;
                let mut items = a.borrow().clone();
                items.reverse();
                Ok(Value::array(items))
            }
            "concat" => {
                let a = want_array(&arg(args, 0), span, &ctx("concat"))?;
                let mut out = a.borrow().clone();
                for (i, extra) in args.iter().enumerate().skip(1) {
                    let more = want_array(extra, span, &format!("array.concat arg {}", i))?;
                    out.extend(more.borrow().iter().cloned());
                }
                Ok(Value::array(out))
            }
            "first" => {
                let a = want_array(&arg(args, 0), span, &ctx("first"))?;
                let val = a.borrow().first().cloned().unwrap_or(Value::nil());
                Ok(val)
            }
            "last" => {
                let a = want_array(&arg(args, 0), span, &ctx("last"))?;
                let val = a.borrow().last().cloned().unwrap_or(Value::nil());
                Ok(val)
            }
            "unique" => {
                let a = want_array(&arg(args, 0), span, &ctx("unique"))?;
                let mut out: Vec<Value> = Vec::new();
                for item in a.borrow().iter() {
                    if !out.contains(item) {
                        out.push(item.clone());
                    }
                }
                Ok(Value::array(out))
            }
            "take" => {
                let a = want_array(&arg(args, 0), span, &ctx("take"))?;
                let nf = want_number(&arg(args, 1), span, &ctx("take"))?;
                let k = if nf < 0.0 {
                    0
                } else {
                    (nf as usize).min(a.borrow().len())
                };
                let out = a.borrow()[..k].to_vec();
                Ok(Value::array(out))
            }
            "drop" => {
                let a = want_array(&arg(args, 0), span, &ctx("drop"))?;
                let nf = want_number(&arg(args, 1), span, &ctx("drop"))?;
                let k = if nf < 0.0 {
                    0
                } else {
                    (nf as usize).min(a.borrow().len())
                };
                let out = a.borrow()[k..].to_vec();
                Ok(Value::array(out))
            }
            "chunk" => {
                let a = want_array(&arg(args, 0), span, &ctx("chunk"))?;
                let nf = want_number(&arg(args, 1), span, &ctx("chunk"))?;
                if nf < 1.0 || nf.fract() != 0.0 {
                    return Err(
                        AsError::at("array.chunk size must be a positive integer", span).into(),
                    );
                }
                let n = nf as usize;
                let out: Vec<Value> = a
                    .borrow()
                    .chunks(n)
                    .map(|c| Value::array(c.to_vec()))
                    .collect();
                Ok(Value::array(out))
            }
            "zip" => {
                if args.is_empty() {
                    return Err(AsError::at("array.zip requires at least one array", span).into());
                }
                let mut cols: Vec<Vec<Value>> = Vec::with_capacity(args.len());
                for (i, v) in args.iter().enumerate() {
                    cols.push(
                        want_array(v, span, &format!("array.zip arg {}", i))?
                            .borrow()
                            .clone(),
                    );
                }
                let len = cols.iter().map(|c| c.len()).min().unwrap_or(0);
                let mut out = Vec::with_capacity(len);
                for i in 0..len {
                    let tuple: Vec<Value> = cols.iter().map(|c| c[i].clone()).collect();
                    out.push(Value::array(tuple));
                }
                Ok(Value::array(out))
            }
            "groupBy" => {
                let a = want_array(&arg(args, 0), span, &ctx("groupBy"))?;
                let f = arg(args, 1);
                let items = a.borrow().clone();
                let mut cb = self.callback_driver(f, span);
                let mut groups: IndexMap<MapKey, Vec<Value>> = IndexMap::new();
                for item in items.into_iter() {
                    let key = cb.call1(item.clone()).await?;
                    let mk = MapKey::from_value(&key).ok_or_else(|| -> Control {
                        AsError::at("array.groupBy key must be a string, number, or bool", span)
                            .into()
                    })?;
                    groups.entry(mk).or_default().push(item);
                }
                let map: IndexMap<MapKey, Value> = groups
                    .into_iter()
                    .map(|(k, v)| (k, Value::array(v)))
                    .collect();
                Ok(Value::map(map))
            }
            "partition" => {
                let a = want_array(&arg(args, 0), span, &ctx("partition"))?;
                let f = arg(args, 1);
                let items = a.borrow().clone();
                let mut cb = self.callback_driver(f, span);
                let (mut pass, mut fail) = (Vec::new(), Vec::new());
                for item in items.into_iter() {
                    if cb.call1(item.clone()).await?.is_truthy() {
                        pass.push(item);
                    } else {
                        fail.push(item);
                    }
                }
                Ok(Value::array(vec![
                    Value::array(pass),
                    Value::array(fail),
                ]))
            }
            _ => Err(AsError::at(format!("std/array has no function '{}'", func), span).into()),
        }
    }
}

/// Flatten `items` to `depth` levels into `out`.
fn flatten_into(items: &[Value], depth: usize, out: &mut Vec<Value>) {
    for item in items {
        match item.kind() {
            ValueKind::Array(inner) if depth > 0 => {
                flatten_into(&inner.borrow(), depth - 1, out)
            }
            _ => out.push(item.clone()),
        }
    }
}

/// Sort a homogeneous number or string array by natural order. Mixed/other kinds → panic.
fn sort_default(items: &mut [Value], span: Span) -> Result<(), Control> {
    // NUM §4: a number array may mix `Int` and `Float`; both count as numbers and
    // sort by their f64 value (cross-type ordering is well-defined).
    let all_numbers = items.iter().all(|v| v.is_number());
    let all_strings = items.iter().all(|v| matches!(v.kind(), ValueKind::Str(_)));
    if all_numbers {
        items.sort_by(|a, b| match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
            _ => std::cmp::Ordering::Equal,
        });
        Ok(())
    } else if all_strings {
        items.sort_by(|a, b| match (a.kind(), b.kind()) {
            (ValueKind::Str(x), ValueKind::Str(y)) => x.cmp(y),
            _ => std::cmp::Ordering::Equal,
        });
        Ok(())
    } else {
        Err(AsError::at(
            "array.sort without a comparator requires a homogeneous array of numbers or strings",
            span,
        )
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::new(0, 0)
    }
    fn n(x: f64) -> Value {
        Value::float(x)
    }
    fn arr(xs: Vec<Value>) -> Value {
        Value::array(xs)
    }

    /// Compile a single AScript expression (e.g. an arrow function) to a runtime
    /// `Value` so callback-driven array fns can be exercised in-process. Uses the
    /// repo's established lex → parse → exec test idiom (cf. the net_tcp/process
    /// test modules). Synchronous arrow callbacks run inline, so a plain `Interp`
    /// (no `LocalSet`/`install_self`) suffices.
    async fn val(interp: &Interp, src: &str) -> Value {
        let program = format!("let __v = {};", src);
        let tokens = crate::lexer::lex(&program).expect("lex");
        let stmts = crate::parser::parse(&tokens).expect("parse");
        let env = crate::interp::global_env().child();
        interp.exec(&stmts, &env).await.expect("exec");
        env.get("__v").expect("binding")
    }

    #[tokio::test]
    async fn push_mutates_and_returns_len() {
        let interp = Interp::new();
        let a = arr(vec![n(1.0), n(2.0)]);
        let len = interp
            .call_array("push", &[a.clone(), n(3.0)], sp())
            .await
            .unwrap();
        assert_eq!(len, n(3.0));
        assert_eq!(a.to_string(), "[1.0, 2.0, 3.0]");
    }

    #[tokio::test]
    async fn pop_returns_last_then_nil() {
        let interp = Interp::new();
        let a = arr(vec![n(1.0)]);
        assert_eq!(
            interp
                .call_array("pop", std::slice::from_ref(&a), sp())
                .await
                .unwrap(),
            n(1.0)
        );
        assert_eq!(
            interp
                .call_array("pop", std::slice::from_ref(&a), sp())
                .await
                .unwrap(),
            Value::nil()
        );
    }

    #[tokio::test]
    async fn get_handles_oob_negative_and_fractional() {
        let interp = Interp::new();
        let a = arr(vec![n(10.0), n(20.0)]);
        assert_eq!(
            interp
                .call_array("get", &[a.clone(), n(0.0)], sp())
                .await
                .unwrap(),
            n(10.0)
        );
        assert_eq!(
            interp
                .call_array("get", &[a.clone(), n(9.0)], sp())
                .await
                .unwrap(),
            Value::nil()
        );
        assert_eq!(
            interp
                .call_array("get", &[a.clone(), n(-1.0)], sp())
                .await
                .unwrap(),
            Value::nil()
        );
        assert_eq!(
            interp
                .call_array("get", &[a.clone(), n(1.5)], sp())
                .await
                .unwrap(),
            Value::nil()
        );
    }

    #[tokio::test]
    async fn contains_uses_structural_eq() {
        let interp = Interp::new();
        let a = arr(vec![n(1.0), n(2.0)]);
        assert_eq!(
            interp
                .call_array("contains", &[a.clone(), n(2.0)], sp())
                .await
                .unwrap(),
            Value::bool_(true)
        );
        assert_eq!(
            interp
                .call_array("contains", &[a.clone(), n(5.0)], sp())
                .await
                .unwrap(),
            Value::bool_(false)
        );
    }

    #[tokio::test]
    async fn slice_supports_negatives_and_default_end() {
        let interp = Interp::new();
        let a = arr(vec![n(10.0), n(20.0), n(30.0), n(40.0)]);
        assert_eq!(
            interp
                .call_array("slice", &[a.clone(), n(1.0), n(3.0)], sp())
                .await
                .unwrap()
                .to_string(),
            "[20.0, 30.0]"
        );
        assert_eq!(
            interp
                .call_array("slice", &[a.clone(), n(-2.0)], sp())
                .await
                .unwrap()
                .to_string(),
            "[30.0, 40.0]"
        );
        // start >= end → empty
        assert_eq!(
            interp
                .call_array("slice", &[a.clone(), n(3.0), n(1.0)], sp())
                .await
                .unwrap()
                .to_string(),
            "[]"
        );
    }

    #[tokio::test]
    async fn sort_default_rejects_mixed() {
        let interp = Interp::new();
        let a = arr(vec![n(1.0), Value::str("x")]);
        assert!(matches!(
            interp.call_array("sort", &[a], sp()).await,
            Err(Control::Panic(_))
        ));
    }

    #[tokio::test]
    async fn misuse_panics() {
        let interp = Interp::new();
        assert!(matches!(
            interp.call_array("push", &[n(1.0), n(2.0)], sp()).await,
            Err(Control::Panic(_))
        ));
        assert!(matches!(
            interp.call_array("nope", &[arr(vec![])], sp()).await,
            Err(Control::Panic(_))
        ));
    }

    #[tokio::test]
    async fn array_predicates_and_indexof() {
        let interp = Interp::new();
        let a = arr(vec![n(1.0), n(2.0), n(3.0)]);
        assert_eq!(
            interp
                .call_array("indexOf", &[a.clone(), n(2.0)], sp())
                .await
                .unwrap(),
            n(1.0)
        );
        assert_eq!(
            interp
                .call_array("indexOf", &[a.clone(), n(9.0)], sp())
                .await
                .unwrap(),
            n(-1.0)
        );
    }

    #[tokio::test]
    async fn array_grouping() {
        let interp = Interp::new();
        let a = arr(vec![n(1.0), n(2.0), n(3.0), n(4.0), n(5.0)]);
        assert_eq!(
            interp
                .call_array("chunk", &[a.clone(), n(2.0)], sp())
                .await
                .unwrap()
                .to_string(),
            "[[1.0, 2.0], [3.0, 4.0], [5.0]]"
        );
        let b = arr(vec![n(10.0), n(20.0)]);
        assert_eq!(
            interp
                .call_array("zip", &[a.clone(), b], sp())
                .await
                .unwrap()
                .to_string(),
            "[[1.0, 10.0], [2.0, 20.0]]"
        );
        assert!(matches!(
            interp.call_array("chunk", &[a.clone(), n(0.0)], sp()).await,
            Err(Control::Panic(_))
        ));
    }

    #[tokio::test]
    async fn group_by_callback() {
        let interp = Interp::new();
        let a = arr(vec![n(1.0), n(2.0), n(3.0), n(4.0), n(5.0)]);
        // Group by parity; the IndexMap preserves first-seen key order ("odd" then "even").
        let f = val(&interp, r#"(x) => x % 2 == 0 ? "even" : "odd""#).await;
        let result = interp.call_array("groupBy", &[a, f], sp()).await.unwrap();
        let map_cell = match result.kind() {
            ValueKind::Map(m) => m.clone(),
            _ => panic!("expected map, got {}", result),
        };
        let map = map_cell.borrow();
        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get(&MapKey::from_value(&Value::str("odd")).unwrap())
                .unwrap()
                .to_string(),
            "[1.0, 3.0, 5.0]"
        );
        assert_eq!(
            map.get(&MapKey::from_value(&Value::str("even")).unwrap())
                .unwrap()
                .to_string(),
            "[2.0, 4.0]"
        );
        // Full stringification confirms insertion order.
        drop(map);
        assert_eq!(
            result.to_string(),
            r#"map {"odd": [1.0, 3.0, 5.0], "even": [2.0, 4.0]}"#
        );
    }

    #[tokio::test]
    async fn group_by_non_hashable_key_panics() {
        let interp = Interp::new();
        let a = arr(vec![n(1.0)]);
        // A callback returning an array yields a non-hashable key → Tier-2 panic.
        let f = val(&interp, "(x) => [x]").await;
        assert!(matches!(
            interp.call_array("groupBy", &[a, f], sp()).await,
            Err(Control::Panic(_))
        ));
    }

    #[tokio::test]
    async fn partition_predicate() {
        let interp = Interp::new();
        let a = arr(vec![n(1.0), n(2.0), n(3.0), n(4.0), n(5.0)]);
        let even = val(&interp, "(x) => x % 2 == 0").await;
        assert_eq!(
            interp
                .call_array("partition", &[a, even.clone()], sp())
                .await
                .unwrap()
                .to_string(),
            "[[2.0, 4.0], [1.0, 3.0, 5.0]]"
        );
        // Empty input → two empty partitions.
        assert_eq!(
            interp
                .call_array("partition", &[arr(vec![]), even], sp())
                .await
                .unwrap()
                .to_string(),
            "[[], []]"
        );
    }

    #[tokio::test]
    async fn array_structural() {
        let interp = Interp::new();
        let a = arr(vec![n(1.0), n(2.0), n(2.0), n(3.0)]);
        assert_eq!(
            interp
                .call_array("reverse", std::slice::from_ref(&a), sp())
                .await
                .unwrap()
                .to_string(),
            "[3.0, 2.0, 2.0, 1.0]"
        );
        assert_eq!(
            interp
                .call_array("unique", std::slice::from_ref(&a), sp())
                .await
                .unwrap()
                .to_string(),
            "[1.0, 2.0, 3.0]"
        );
        assert_eq!(
            interp
                .call_array("first", std::slice::from_ref(&a), sp())
                .await
                .unwrap(),
            n(1.0)
        );
        assert_eq!(
            interp
                .call_array("last", std::slice::from_ref(&a), sp())
                .await
                .unwrap(),
            n(3.0)
        );
        assert_eq!(
            interp
                .call_array("first", &[arr(vec![])], sp())
                .await
                .unwrap(),
            Value::nil()
        );
        assert_eq!(
            interp
                .call_array("take", &[a.clone(), n(2.0)], sp())
                .await
                .unwrap()
                .to_string(),
            "[1.0, 2.0]"
        );
        assert_eq!(
            interp
                .call_array("drop", &[a.clone(), n(2.0)], sp())
                .await
                .unwrap()
                .to_string(),
            "[2.0, 3.0]"
        );
        let nested = arr(vec![arr(vec![n(1.0)]), arr(vec![n(2.0), n(3.0)])]);
        assert_eq!(
            interp
                .call_array("flat", std::slice::from_ref(&nested), sp())
                .await
                .unwrap()
                .to_string(),
            "[1.0, 2.0, 3.0]"
        );
        let b = arr(vec![n(4.0)]);
        assert_eq!(
            interp
                .call_array("concat", &[arr(vec![n(1.0)]), b], sp())
                .await
                .unwrap()
                .to_string(),
            "[1.0, 4.0]"
        );
    }
}
