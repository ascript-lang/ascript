//! `std/array` — array operations. Callback-taking functions (`map`, `filter`,
//! `reduce`, `sort`) live on `impl Interp` because they invoke user functions.

use super::{arg, bi, clamp_index, want_array, want_number};
use crate::error::AsError;
use crate::interp::{Control, Interp};
use crate::span::Span;
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

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
    ]
}

impl Interp {
    pub(crate) async fn call_array(&mut self, func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
        let ctx = |f: &str| format!("array.{}", f);
        match func {
            "map" => {
                let arr = want_array(&arg(args, 0), span, &ctx("map"))?;
                let f = arg(args, 1);
                let items = arr.borrow().clone();
                let mut out = Vec::with_capacity(items.len());
                for item in items.into_iter() {
                    let v = self.call_value(f.clone(), vec![item], span).await?;
                    out.push(v);
                }
                Ok(Value::Array(Rc::new(RefCell::new(out))))
            }
            "filter" => {
                let arr = want_array(&arg(args, 0), span, &ctx("filter"))?;
                let f = arg(args, 1);
                let items = arr.borrow().clone();
                let mut out = Vec::new();
                for item in items.into_iter() {
                    let keep = self.call_value(f.clone(), vec![item.clone()], span).await?;
                    if keep.is_truthy() {
                        out.push(item);
                    }
                }
                Ok(Value::Array(Rc::new(RefCell::new(out))))
            }
            "reduce" => {
                let arr = want_array(&arg(args, 0), span, &ctx("reduce"))?;
                let f = arg(args, 1);
                let mut acc = arg(args, 2);
                let items = arr.borrow().clone();
                for item in items.into_iter() {
                    acc = self.call_value(f.clone(), vec![acc, item], span).await?;
                }
                Ok(acc)
            }
            "push" => {
                let arr = want_array(&arg(args, 0), span, &ctx("push"))?;
                let item = arg(args, 1);
                let mut b = arr.borrow_mut();
                b.push(item);
                Ok(Value::Number(b.len() as f64))
            }
            "pop" => {
                let arr = want_array(&arg(args, 0), span, &ctx("pop"))?;
                let popped = arr.borrow_mut().pop();
                Ok(popped.unwrap_or(Value::Nil))
            }
            "slice" => {
                let arr = want_array(&arg(args, 0), span, &ctx("slice"))?;
                let b = arr.borrow();
                let len = b.len();
                let start = clamp_index(want_number(&arg(args, 1), span, &ctx("slice"))?, len);
                let end = match args.get(2) {
                    None | Some(Value::Nil) => len,
                    Some(v) => clamp_index(want_number(v, span, &ctx("slice"))?, len),
                };
                let out: Vec<Value> = if start < end { b[start..end].to_vec() } else { Vec::new() };
                Ok(Value::Array(Rc::new(RefCell::new(out))))
            }
            "contains" => {
                let arr = want_array(&arg(args, 0), span, &ctx("contains"))?;
                let needle = arg(args, 1);
                let found = arr.borrow().contains(&needle);
                Ok(Value::Bool(found))
            }
            "get" => {
                let arr = want_array(&arg(args, 0), span, &ctx("get"))?;
                let i = want_number(&arg(args, 1), span, &ctx("get"))?;
                if i < 0.0 || i.fract() != 0.0 {
                    return Ok(Value::Nil);
                }
                let got = arr.borrow().get(i as usize).cloned();
                Ok(got.unwrap_or(Value::Nil))
            }
            "sort" => {
                let arr = want_array(&arg(args, 0), span, &ctx("sort"))?;
                let mut items = arr.borrow().clone();
                let cmp = args.get(1).cloned();
                match cmp {
                    Some(Value::Nil) | None => {
                        sort_default(&mut items, span)?;
                    }
                    Some(f) => {
                        // Insertion sort driven by the async comparator: insert each
                        // item before the first existing element it compares < 0 against.
                        // O(n^2), acceptable because each comparison is an async user-
                        // callback call (a standard borrow-free sort is awkward with an
                        // async comparator). Stable: insertion stops at the first strictly
                        // negative compare, so equal-key elements keep their input order.
                        let mut sorted: Vec<Value> = Vec::with_capacity(items.len());
                        for item in items.into_iter() {
                            let mut lo = 0usize;
                            while lo < sorted.len() {
                                let r = self.call_value(f.clone(), vec![item.clone(), sorted[lo].clone()], span).await?;
                                let n = match r {
                                    Value::Number(n) => n,
                                    other => return Err(AsError::at(
                                        format!("array.sort comparator must return a number, got {}", crate::interp::type_name(&other)),
                                        span,
                                    ).into()),
                                };
                                if n < 0.0 { break; }
                                lo += 1;
                            }
                            sorted.insert(lo, item);
                        }
                        items = sorted;
                    }
                }
                Ok(Value::Array(Rc::new(RefCell::new(items))))
            }
            _ => Err(AsError::at(format!("std/array has no function '{}'", func), span).into()),
        }
    }
}

/// Sort a homogeneous number or string array by natural order. Mixed/other kinds → panic.
fn sort_default(items: &mut [Value], span: Span) -> Result<(), Control> {
    let all_numbers = items.iter().all(|v| matches!(v, Value::Number(_)));
    let all_strings = items.iter().all(|v| matches!(v, Value::Str(_)));
    if all_numbers {
        items.sort_by(|a, b| match (a, b) {
            (Value::Number(x), Value::Number(y)) => x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
            _ => std::cmp::Ordering::Equal,
        });
        Ok(())
    } else if all_strings {
        items.sort_by(|a, b| match (a, b) {
            (Value::Str(x), Value::Str(y)) => x.cmp(y),
            _ => std::cmp::Ordering::Equal,
        });
        Ok(())
    } else {
        Err(AsError::at("array.sort without a comparator requires a homogeneous array of numbers or strings", span).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::new(0, 0)
    }
    fn n(x: f64) -> Value {
        Value::Number(x)
    }
    fn arr(xs: Vec<Value>) -> Value {
        Value::Array(Rc::new(RefCell::new(xs)))
    }

    #[tokio::test]
    async fn push_mutates_and_returns_len() {
        let mut interp = Interp::new();
        let a = arr(vec![n(1.0), n(2.0)]);
        let len = interp.call_array("push", &[a.clone(), n(3.0)], sp()).await.unwrap();
        assert_eq!(len, n(3.0));
        assert_eq!(a.to_string(), "[1, 2, 3]");
    }

    #[tokio::test]
    async fn pop_returns_last_then_nil() {
        let mut interp = Interp::new();
        let a = arr(vec![n(1.0)]);
        assert_eq!(interp.call_array("pop", std::slice::from_ref(&a), sp()).await.unwrap(), n(1.0));
        assert_eq!(interp.call_array("pop", std::slice::from_ref(&a), sp()).await.unwrap(), Value::Nil);
    }

    #[tokio::test]
    async fn get_handles_oob_negative_and_fractional() {
        let mut interp = Interp::new();
        let a = arr(vec![n(10.0), n(20.0)]);
        assert_eq!(interp.call_array("get", &[a.clone(), n(0.0)], sp()).await.unwrap(), n(10.0));
        assert_eq!(interp.call_array("get", &[a.clone(), n(9.0)], sp()).await.unwrap(), Value::Nil);
        assert_eq!(interp.call_array("get", &[a.clone(), n(-1.0)], sp()).await.unwrap(), Value::Nil);
        assert_eq!(interp.call_array("get", &[a.clone(), n(1.5)], sp()).await.unwrap(), Value::Nil);
    }

    #[tokio::test]
    async fn contains_uses_structural_eq() {
        let mut interp = Interp::new();
        let a = arr(vec![n(1.0), n(2.0)]);
        assert_eq!(interp.call_array("contains", &[a.clone(), n(2.0)], sp()).await.unwrap(), Value::Bool(true));
        assert_eq!(interp.call_array("contains", &[a.clone(), n(5.0)], sp()).await.unwrap(), Value::Bool(false));
    }

    #[tokio::test]
    async fn slice_supports_negatives_and_default_end() {
        let mut interp = Interp::new();
        let a = arr(vec![n(10.0), n(20.0), n(30.0), n(40.0)]);
        assert_eq!(interp.call_array("slice", &[a.clone(), n(1.0), n(3.0)], sp()).await.unwrap().to_string(), "[20, 30]");
        assert_eq!(interp.call_array("slice", &[a.clone(), n(-2.0)], sp()).await.unwrap().to_string(), "[30, 40]");
        // start >= end → empty
        assert_eq!(interp.call_array("slice", &[a.clone(), n(3.0), n(1.0)], sp()).await.unwrap().to_string(), "[]");
    }

    #[tokio::test]
    async fn sort_default_rejects_mixed() {
        let mut interp = Interp::new();
        let a = arr(vec![n(1.0), Value::Str("x".into())]);
        assert!(matches!(interp.call_array("sort", &[a], sp()).await, Err(Control::Panic(_))));
    }

    #[tokio::test]
    async fn misuse_panics() {
        let mut interp = Interp::new();
        assert!(matches!(interp.call_array("push", &[n(1.0), n(2.0)], sp()).await, Err(Control::Panic(_))));
        assert!(matches!(interp.call_array("nope", &[arr(vec![])], sp()).await, Err(Control::Panic(_))));
    }
}
