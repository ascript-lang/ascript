# Phase 1 — Stdlib Completeness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fill out `array`, `string`, `math`, `object` with everyday methods plus non-crypto checksums, all as additive native functions, with one pre-audited `string.replace` breaking change.

**Architecture:** Each `std/*` module is native Rust over `Value`. Pure functions live in the module's `call(func, args, span)`; callback-taking functions live on `impl Interp` (async, call user functions). New pure fns go in the respective module `call`; the one new callback fn (`object.mapValues`) gets a new `impl Interp::call_object` mirroring the existing `call_array`, plus a one-line routing change in `mod.rs`. Every new function must also be added to its module's `exports()` so `import` brings it in.

**Tech Stack:** Rust, `IndexMap` (objects/maps), `Rc<RefCell<…>>` containers, `crc32fast` + `xxhash-rust` crates (behind the existing `crypto` feature).

**Conventions (match existing code):** non-mutating transforms return new arrays; type misuse → Tier-2 panic via `want_*`; not-found → `nil`, index-not-found → `-1`; strings are char-indexed. Helpers in `src/stdlib/mod.rs`: `arg(args, i)`, `bi("mod.fn")`, `want_array`/`want_string`/`want_number`/`want_object`/`want_bytes`, `clamp_index(i, len)`. Construct arrays with `Value::Array(Rc::new(RefCell::new(vec)))`.

**Build/test commands:** `cargo test <name>` (single), `cargo test` (full), `cargo clippy --all-targets` and `cargo clippy --no-default-features --all-targets` (both must be clean).

---

## Task 1: `string.replace` breaking change + `replaceAll`

**Files:**
- Modify: `src/stdlib/string.rs` (exports + `call` arm `replace`, add `replaceAll`)
- Modify test: `src/stdlib/string.rs` (tests at `:173`, `:191`)
- Modify doc: `docs/content/stdlib/collections.md:121`

- [ ] **Step 1: Update the existing tests to the new semantics (failing)**

In `src/stdlib/string.rs` replace the line at `:173` and add `replaceAll` coverage:

```rust
// replace = FIRST occurrence only
assert_eq!(call("replace", &[s("a.b.c"), s("."), s("-")], sp()).unwrap(), s("a-b.c"));
// replaceAll = all occurrences
assert_eq!(call("replaceAll", &[s("a.b.c"), s("."), s("-")], sp()).unwrap(), s("a-b-c"));
// empty `from` leaves input unchanged for both
assert_eq!(call("replace", &[s("abc"), s(""), s("X")], sp()).unwrap(), s("abc"));
assert_eq!(call("replaceAll", &[s("abc"), s(""), s("X")], sp()).unwrap(), s("abc"));
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib stdlib::string`
Expected: FAIL (`replaceAll` not found; `replace` still replaces all → `a-b-c` != `a-b.c`).

- [ ] **Step 3: Implement**

In `exports()` add after the `replace` entry:
```rust
("replaceAll", bi("string.replaceAll")),
```
Change the `"replace"` arm body (`:78`) and add a `"replaceAll"` arm:
```rust
"replace" => {
    let s = want_string(&arg(args, 0), span, &ctx("replace"))?;
    let from = want_string(&arg(args, 1), span, &ctx("replace"))?;
    let to = want_string(&arg(args, 2), span, &ctx("replace"))?;
    let result = if from.is_empty() { s.to_string() } else { s.replacen(from.as_ref(), to.as_ref(), 1) };
    Ok(Value::Str(result.into()))
}
"replaceAll" => {
    let s = want_string(&arg(args, 0), span, &ctx("replaceAll"))?;
    let from = want_string(&arg(args, 1), span, &ctx("replaceAll"))?;
    let to = want_string(&arg(args, 2), span, &ctx("replaceAll"))?;
    let result = if from.is_empty() { s.to_string() } else { s.replace(from.as_ref(), to.as_ref()) };
    Ok(Value::Str(result.into()))
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib stdlib::string`
Expected: PASS.

- [ ] **Step 5: Update the doc example**

In `docs/content/stdlib/collections.md:121`, change to:
```
string.replace("a.b.c", ".", "-")      // "a-b.c"  (first only)
string.replaceAll("a.b.c", ".", "-")   // "a-b-c"  (all)
```

- [ ] **Step 6: Commit**

```bash
git add src/stdlib/string.rs docs/content/stdlib/collections.md
git commit -m "feat(string): replace = first occurrence; add replaceAll"
```

---

## Task 2: `string` completeness

**Files:**
- Modify: `src/stdlib/string.rs` (exports + new `call` arms + tests)

- [ ] **Step 1: Write failing tests**

Add to the `tests` module in `src/stdlib/string.rs`:
```rust
#[test]
fn string_completeness() {
    assert_eq!(call("startsWith", &[s("hello"), s("he")], sp()).unwrap(), Value::Bool(true));
    assert_eq!(call("endsWith", &[s("hello"), s("lo")], sp()).unwrap(), Value::Bool(true));
    assert_eq!(call("contains", &[s("hello"), s("ell")], sp()).unwrap(), Value::Bool(true));
    assert_eq!(call("contains", &[s("hello"), s("")], sp()).unwrap(), Value::Bool(true));
    assert_eq!(call("chars", &[s("ab")], sp()).unwrap().to_string(), "[a, b]");
    assert_eq!(call("lines", &[s("a\nb\n")], sp()).unwrap().to_string(), "[a, b]");
    assert_eq!(call("reverse", &[s("abc")], sp()).unwrap(), s("cba"));
    assert_eq!(call("count", &[s("a.a.a"), s(".")], sp()).unwrap(), Value::Number(2.0));
    assert_eq!(call("count", &[s("abc"), s("")], sp()).unwrap(), Value::Number(0.0));
    assert_eq!(call("splitN", &[s("a:b:c"), s(":"), Value::Number(2.0)], sp()).unwrap().to_string(), "[a, b:c]");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib stdlib::string::tests::string_completeness`
Expected: FAIL (functions not found).

- [ ] **Step 3: Implement**

Add to `exports()`:
```rust
("startsWith", bi("string.startsWith")),
("endsWith", bi("string.endsWith")),
("contains", bi("string.contains")),
("chars", bi("string.chars")),
("lines", bi("string.lines")),
("reverse", bi("string.reverse")),
("count", bi("string.count")),
("splitN", bi("string.splitN")),
```
Add arms in `call` (before the `_ =>` fallback):
```rust
"startsWith" => {
    let s = want_string(&arg(args, 0), span, &ctx("startsWith"))?;
    let p = want_string(&arg(args, 1), span, &ctx("startsWith"))?;
    Ok(Value::Bool(s.starts_with(p.as_ref())))
}
"endsWith" => {
    let s = want_string(&arg(args, 0), span, &ctx("endsWith"))?;
    let p = want_string(&arg(args, 1), span, &ctx("endsWith"))?;
    Ok(Value::Bool(s.ends_with(p.as_ref())))
}
"contains" => {
    let s = want_string(&arg(args, 0), span, &ctx("contains"))?;
    let sub = want_string(&arg(args, 1), span, &ctx("contains"))?;
    Ok(Value::Bool(s.contains(sub.as_ref())))
}
"chars" => {
    let s = want_string(&arg(args, 0), span, &ctx("chars"))?;
    let out: Vec<Value> = s.chars().map(|c| Value::Str(c.to_string().into())).collect();
    Ok(Value::Array(Rc::new(RefCell::new(out))))
}
"lines" => {
    let s = want_string(&arg(args, 0), span, &ctx("lines"))?;
    let out: Vec<Value> = s.lines().map(|l| Value::Str(l.into())).collect();
    Ok(Value::Array(Rc::new(RefCell::new(out))))
}
"reverse" => {
    let s = want_string(&arg(args, 0), span, &ctx("reverse"))?;
    Ok(Value::Str(s.chars().rev().collect::<String>().into()))
}
"count" => {
    let s = want_string(&arg(args, 0), span, &ctx("count"))?;
    let sub = want_string(&arg(args, 1), span, &ctx("count"))?;
    let n = if sub.is_empty() { 0 } else { s.matches(sub.as_ref()).count() };
    Ok(Value::Number(n as f64))
}
"splitN" => {
    let s = want_string(&arg(args, 0), span, &ctx("splitN"))?;
    let sep = want_string(&arg(args, 1), span, &ctx("splitN"))?;
    let n = want_number(&arg(args, 2), span, &ctx("splitN"))?.max(0.0) as usize;
    let out: Vec<Value> = s.splitn(n, sep.as_ref()).map(|p| Value::Str(p.into())).collect();
    Ok(Value::Array(Rc::new(RefCell::new(out))))
}
```
Ensure `use std::cell::RefCell; use std::rc::Rc;` are imported in `string.rs` (add if missing).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib stdlib::string`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/stdlib/string.rs
git commit -m "feat(string): startsWith/endsWith/contains/chars/lines/reverse/count/splitN"
```

---

## Task 3: `array` predicates & search (callback + indexOf)

**Files:**
- Modify: `src/stdlib/array.rs` (exports + arms in `call_array` + tests)

- [ ] **Step 1: Write failing tests**

Add to the `tests` module in `src/stdlib/array.rs`:
```rust
#[tokio::test]
async fn array_predicates_and_indexof() {
    let interp = Interp::new();
    let a = arr(vec![n(1.0), n(2.0), n(3.0)]);
    // gt2 = a closure returning bool-ish; reuse a script fn via eval is heavy, so use a builtin-like:
    // Use math via call_value is unavailable here; instead test indexOf (no callback) and the
    // callback fns through a simple identity-style closure built in interp tests is not trivial.
    assert_eq!(interp.call_array("indexOf", &[a.clone(), n(2.0)], sp()).await.unwrap(), n(1.0));
    assert_eq!(interp.call_array("indexOf", &[a.clone(), n(9.0)], sp()).await.unwrap(), n(-1.0));
}
```

> NOTE: callback fns (`find`/`findIndex`/`some`/`every`) are integration-tested via `.as` programs in Task 12's example and the conformance tests, because constructing a `Value::Function` in a Rust unit test is awkward. The unit test above covers the non-callback `indexOf`; callback arms are covered end-to-end.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib stdlib::array::tests::array_predicates_and_indexof`
Expected: FAIL (`indexOf` not found).

- [ ] **Step 3: Implement**

Add to `exports()`:
```rust
("find", bi("array.find")),
("findIndex", bi("array.findIndex")),
("some", bi("array.some")),
("every", bi("array.every")),
("indexOf", bi("array.indexOf")),
```
Add arms in `call_array` (before `_ =>`):
```rust
"find" => {
    let a = want_array(&arg(args, 0), span, &ctx("find"))?;
    let f = arg(args, 1);
    let items = a.borrow().clone();
    for item in items.into_iter() {
        if self.call_value(f.clone(), vec![item.clone()], span).await?.is_truthy() {
            return Ok(item);
        }
    }
    Ok(Value::Nil)
}
"findIndex" => {
    let a = want_array(&arg(args, 0), span, &ctx("findIndex"))?;
    let f = arg(args, 1);
    let items = a.borrow().clone();
    for (i, item) in items.into_iter().enumerate() {
        if self.call_value(f.clone(), vec![item], span).await?.is_truthy() {
            return Ok(Value::Number(i as f64));
        }
    }
    Ok(Value::Number(-1.0))
}
"some" => {
    let a = want_array(&arg(args, 0), span, &ctx("some"))?;
    let f = arg(args, 1);
    let items = a.borrow().clone();
    for item in items.into_iter() {
        if self.call_value(f.clone(), vec![item], span).await?.is_truthy() {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}
"every" => {
    let a = want_array(&arg(args, 0), span, &ctx("every"))?;
    let f = arg(args, 1);
    let items = a.borrow().clone();
    for item in items.into_iter() {
        if !self.call_value(f.clone(), vec![item], span).await?.is_truthy() {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}
"indexOf" => {
    let a = want_array(&arg(args, 0), span, &ctx("indexOf"))?;
    let needle = arg(args, 1);
    let idx = a.borrow().iter().position(|x| *x == needle);
    Ok(Value::Number(idx.map(|i| i as f64).unwrap_or(-1.0)))
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib stdlib::array`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/stdlib/array.rs
git commit -m "feat(array): find/findIndex/some/every/indexOf"
```

---

## Task 4: `array` structural transforms

**Files:**
- Modify: `src/stdlib/array.rs` (exports + arms + tests)

- [ ] **Step 1: Write failing tests**

```rust
#[tokio::test]
async fn array_structural() {
    let interp = Interp::new();
    let a = arr(vec![n(1.0), n(2.0), n(2.0), n(3.0)]);
    assert_eq!(interp.call_array("reverse", &[a.clone()], sp()).await.unwrap().to_string(), "[3, 2, 2, 1]");
    assert_eq!(interp.call_array("unique", &[a.clone()], sp()).await.unwrap().to_string(), "[1, 2, 3]");
    assert_eq!(interp.call_array("first", &[a.clone()], sp()).await.unwrap(), n(1.0));
    assert_eq!(interp.call_array("last", &[a.clone()], sp()).await.unwrap(), n(3.0));
    assert_eq!(interp.call_array("first", &[arr(vec![])], sp()).await.unwrap(), Value::Nil);
    assert_eq!(interp.call_array("take", &[a.clone(), n(2.0)], sp()).await.unwrap().to_string(), "[1, 2]");
    assert_eq!(interp.call_array("drop", &[a.clone(), n(2.0)], sp()).await.unwrap().to_string(), "[2, 3]");
    let nested = arr(vec![arr(vec![n(1.0)]), arr(vec![n(2.0), n(3.0)])]);
    assert_eq!(interp.call_array("flat", &[nested.clone()], sp()).await.unwrap().to_string(), "[1, 2, 3]");
    let b = arr(vec![n(4.0)]);
    assert_eq!(interp.call_array("concat", &[arr(vec![n(1.0)]), b], sp()).await.unwrap().to_string(), "[1, 4]");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib stdlib::array::tests::array_structural`
Expected: FAIL.

- [ ] **Step 3: Implement**

Add to `exports()`:
```rust
("flat", bi("array.flat")),
("flatMap", bi("array.flatMap")),
("reverse", bi("array.reverse")),
("concat", bi("array.concat")),
("first", bi("array.first")),
("last", bi("array.last")),
("unique", bi("array.unique")),
("take", bi("array.take")),
("drop", bi("array.drop")),
```
Add a free helper at the bottom of `array.rs` (module scope):
```rust
/// Flatten `items` to `depth` levels into `out`.
fn flatten_into(items: &[Value], depth: usize, out: &mut Vec<Value>) {
    for item in items {
        match item {
            Value::Array(inner) if depth > 0 => flatten_into(&inner.borrow(), depth - 1, out),
            other => out.push(other.clone()),
        }
    }
}
```
Add arms in `call_array`:
```rust
"flat" => {
    let a = want_array(&arg(args, 0), span, &ctx("flat"))?;
    let depth = match args.get(1) {
        None | Some(Value::Nil) => 1usize,
        Some(v) => {
            let d = want_number(v, span, &ctx("flat"))?;
            if d < 0.0 || d.fract() != 0.0 {
                return Err(AsError::at("array.flat depth must be a non-negative integer", span).into());
            }
            d as usize
        }
    };
    let mut out = Vec::new();
    flatten_into(&a.borrow(), depth, &mut out);
    Ok(Value::Array(Rc::new(RefCell::new(out))))
}
"flatMap" => {
    let a = want_array(&arg(args, 0), span, &ctx("flatMap"))?;
    let f = arg(args, 1);
    let items = a.borrow().clone();
    let mut out = Vec::new();
    for item in items.into_iter() {
        let mapped = self.call_value(f.clone(), vec![item], span).await?;
        match mapped {
            Value::Array(inner) => out.extend(inner.borrow().iter().cloned()),
            other => out.push(other),
        }
    }
    Ok(Value::Array(Rc::new(RefCell::new(out))))
}
"reverse" => {
    let a = want_array(&arg(args, 0), span, &ctx("reverse"))?;
    let mut items = a.borrow().clone();
    items.reverse();
    Ok(Value::Array(Rc::new(RefCell::new(items))))
}
"concat" => {
    let a = want_array(&arg(args, 0), span, &ctx("concat"))?;
    let mut out = a.borrow().clone();
    for (i, extra) in args.iter().enumerate().skip(1) {
        let more = want_array(extra, span, &format!("array.concat arg {}", i))?;
        out.extend(more.borrow().iter().cloned());
    }
    Ok(Value::Array(Rc::new(RefCell::new(out))))
}
"first" => {
    let a = want_array(&arg(args, 0), span, &ctx("first"))?;
    Ok(a.borrow().first().cloned().unwrap_or(Value::Nil))
}
"last" => {
    let a = want_array(&arg(args, 0), span, &ctx("last"))?;
    Ok(a.borrow().last().cloned().unwrap_or(Value::Nil))
}
"unique" => {
    let a = want_array(&arg(args, 0), span, &ctx("unique"))?;
    let mut out: Vec<Value> = Vec::new();
    for item in a.borrow().iter() {
        if !out.contains(item) {
            out.push(item.clone());
        }
    }
    Ok(Value::Array(Rc::new(RefCell::new(out))))
}
"take" => {
    let a = want_array(&arg(args, 0), span, &ctx("take"))?;
    let n = want_number(&arg(args, 1), span, &ctx("take"))?;
    let k = if n < 0.0 { 0 } else { (n as usize).min(a.borrow().len()) };
    let out = a.borrow()[..k].to_vec();
    Ok(Value::Array(Rc::new(RefCell::new(out))))
}
"drop" => {
    let a = want_array(&arg(args, 0), span, &ctx("drop"))?;
    let n = want_number(&arg(args, 1), span, &ctx("drop"))?;
    let k = if n < 0.0 { 0 } else { (n as usize).min(a.borrow().len()) };
    let out = a.borrow()[k..].to_vec();
    Ok(Value::Array(Rc::new(RefCell::new(out))))
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib stdlib::array`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/stdlib/array.rs
git commit -m "feat(array): flat/flatMap/reverse/concat/first/last/unique/take/drop"
```

---

## Task 5: `array` grouping (chunk/zip/groupBy/partition)

**Files:**
- Modify: `src/stdlib/array.rs` (exports + arms + tests)

- [ ] **Step 1: Write failing tests**

```rust
#[tokio::test]
async fn array_grouping() {
    let interp = Interp::new();
    let a = arr(vec![n(1.0), n(2.0), n(3.0), n(4.0), n(5.0)]);
    assert_eq!(interp.call_array("chunk", &[a.clone(), n(2.0)], sp()).await.unwrap().to_string(), "[[1, 2], [3, 4], [5]]");
    let b = arr(vec![n(10.0), n(20.0)]);
    assert_eq!(interp.call_array("zip", &[a.clone(), b], sp()).await.unwrap().to_string(), "[[1, 10], [2, 20]]");
    // chunk with n<=0 panics
    assert!(matches!(interp.call_array("chunk", &[a.clone(), n(0.0)], sp()).await, Err(Control::Panic(_))));
}
```

> `groupBy` (returns a `Map`) and `partition` are exercised end-to-end in the Task 12 example (`Map` literals are awkward to assert in a Rust unit test).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib stdlib::array::tests::array_grouping`
Expected: FAIL.

- [ ] **Step 3: Implement**

Add imports at the top of `array.rs`:
```rust
use crate::value::MapKey;
use indexmap::IndexMap;
```
Add to `exports()`:
```rust
("chunk", bi("array.chunk")),
("zip", bi("array.zip")),
("groupBy", bi("array.groupBy")),
("partition", bi("array.partition")),
```
Add arms in `call_array`:
```rust
"chunk" => {
    let a = want_array(&arg(args, 0), span, &ctx("chunk"))?;
    let nf = want_number(&arg(args, 1), span, &ctx("chunk"))?;
    if nf < 1.0 || nf.fract() != 0.0 {
        return Err(AsError::at("array.chunk size must be a positive integer", span).into());
    }
    let n = nf as usize;
    let out: Vec<Value> = a.borrow()
        .chunks(n)
        .map(|c| Value::Array(Rc::new(RefCell::new(c.to_vec()))))
        .collect();
    Ok(Value::Array(Rc::new(RefCell::new(out))))
}
"zip" => {
    if args.is_empty() {
        return Err(AsError::at("array.zip requires at least one array", span).into());
    }
    let mut cols: Vec<Vec<Value>> = Vec::with_capacity(args.len());
    for (i, v) in args.iter().enumerate() {
        cols.push(want_array(v, span, &format!("array.zip arg {}", i))?.borrow().clone());
    }
    let len = cols.iter().map(|c| c.len()).min().unwrap_or(0);
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let tuple: Vec<Value> = cols.iter().map(|c| c[i].clone()).collect();
        out.push(Value::Array(Rc::new(RefCell::new(tuple))));
    }
    Ok(Value::Array(Rc::new(RefCell::new(out))))
}
"groupBy" => {
    let a = want_array(&arg(args, 0), span, &ctx("groupBy"))?;
    let f = arg(args, 1);
    let items = a.borrow().clone();
    let mut groups: IndexMap<MapKey, Vec<Value>> = IndexMap::new();
    for item in items.into_iter() {
        let key = self.call_value(f.clone(), vec![item.clone()], span).await?;
        let mk = MapKey::from_value(&key).ok_or_else(|| -> Control {
            AsError::at("array.groupBy key must be a string, number, or bool", span).into()
        })?;
        groups.entry(mk).or_default().push(item);
    }
    let map: IndexMap<MapKey, Value> = groups
        .into_iter()
        .map(|(k, v)| (k, Value::Array(Rc::new(RefCell::new(v)))))
        .collect();
    Ok(Value::Map(Rc::new(RefCell::new(map))))
}
"partition" => {
    let a = want_array(&arg(args, 0), span, &ctx("partition"))?;
    let f = arg(args, 1);
    let items = a.borrow().clone();
    let (mut pass, mut fail) = (Vec::new(), Vec::new());
    for item in items.into_iter() {
        if self.call_value(f.clone(), vec![item.clone()], span).await?.is_truthy() {
            pass.push(item);
        } else {
            fail.push(item);
        }
    }
    Ok(Value::Array(Rc::new(RefCell::new(vec![
        Value::Array(Rc::new(RefCell::new(pass))),
        Value::Array(Rc::new(RefCell::new(fail))),
    ]))))
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib stdlib::array`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/stdlib/array.rs
git commit -m "feat(array): chunk/zip/groupBy(->Map)/partition"
```

---

## Task 6: `math` trig & exponential

**Files:**
- Modify: `src/stdlib/math.rs` (exports + arms + tests)

- [ ] **Step 1: Write failing tests**

Add to the `tests` module in `src/stdlib/math.rs` (use the module's existing test helpers; if none, add `fn sp() -> Span { Span::new(0,0) }` and `fn n(x: f64) -> Value { Value::Number(x) }`):
```rust
#[test]
fn math_trig_exp() {
    assert_eq!(call("sin", &[n(0.0)], sp()).unwrap(), n(0.0));
    assert_eq!(call("cos", &[n(0.0)], sp()).unwrap(), n(1.0));
    assert_eq!(call("exp", &[n(0.0)], sp()).unwrap(), n(1.0));
    assert_eq!(call("ln", &[n(1.0)], sp()).unwrap(), n(0.0));
    assert_eq!(call("log2", &[n(8.0)], sp()).unwrap(), n(3.0));
    assert_eq!(call("log10", &[n(1000.0)], sp()).unwrap(), n(3.0));
    assert_eq!(call("atan2", &[n(0.0), n(1.0)], sp()).unwrap(), n(0.0));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib stdlib::math::tests::math_trig_exp`
Expected: FAIL.

- [ ] **Step 3: Implement**

Add to `exports()`:
```rust
("sin", bi("math.sin")), ("cos", bi("math.cos")), ("tan", bi("math.tan")),
("asin", bi("math.asin")), ("acos", bi("math.acos")), ("atan", bi("math.atan")),
("atan2", bi("math.atan2")), ("exp", bi("math.exp")),
("ln", bi("math.ln")), ("log2", bi("math.log2")), ("log10", bi("math.log10")),
```
Add arms in `call` (before `_ =>`):
```rust
"sin" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("sin"))?.sin())),
"cos" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("cos"))?.cos())),
"tan" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("tan"))?.tan())),
"asin" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("asin"))?.asin())),
"acos" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("acos"))?.acos())),
"atan" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("atan"))?.atan())),
"atan2" => {
    let y = want_number(&arg(args, 0), span, &ctx("atan2"))?;
    let x = want_number(&arg(args, 1), span, &ctx("atan2"))?;
    Ok(Value::Number(y.atan2(x)))
}
"exp" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("exp"))?.exp())),
"ln" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("ln"))?.ln())),
"log2" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("log2"))?.log2())),
"log10" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("log10"))?.log10())),
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib stdlib::math::tests::math_trig_exp`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/stdlib/math.rs
git commit -m "feat(math): trig (sin/cos/tan/asin/acos/atan/atan2) + exp/ln/log2/log10"
```

---

## Task 7: `math` scalar helpers (sign/trunc/clamp/hypot/gcd/lcm)

**Files:**
- Modify: `src/stdlib/math.rs` (exports + arms + tests + free gcd helper)

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn math_scalar_helpers() {
    assert_eq!(call("sign", &[n(-3.0)], sp()).unwrap(), n(-1.0));
    assert_eq!(call("sign", &[n(0.0)], sp()).unwrap(), n(0.0));
    assert_eq!(call("trunc", &[n(3.7)], sp()).unwrap(), n(3.0));
    assert_eq!(call("clamp", &[n(5.0), n(0.0), n(3.0)], sp()).unwrap(), n(3.0));
    assert_eq!(call("hypot", &[n(3.0), n(4.0)], sp()).unwrap(), n(5.0));
    assert_eq!(call("gcd", &[n(12.0), n(8.0)], sp()).unwrap(), n(4.0));
    assert_eq!(call("lcm", &[n(4.0), n(6.0)], sp()).unwrap(), n(12.0));
    assert!(matches!(call("clamp", &[n(1.0), n(3.0), n(0.0)], sp()), Err(Control::Panic(_))));
    assert!(matches!(call("gcd", &[n(1.5), n(2.0)], sp()), Err(Control::Panic(_))));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib stdlib::math::tests::math_scalar_helpers`
Expected: FAIL.

- [ ] **Step 3: Implement**

Add a free helper at module scope in `math.rs`:
```rust
/// Require `x` to be an integer-valued f64; returns it as i64 or panics.
fn want_int(x: f64, span: Span, ctx: &str) -> Result<i64, Control> {
    if x.fract() != 0.0 || !x.is_finite() {
        return Err(AsError::at(format!("{} requires integer values", ctx), span).into());
    }
    Ok(x as i64)
}
fn gcd_i64(mut a: i64, mut b: i64) -> i64 {
    a = a.abs(); b = b.abs();
    while b != 0 { let t = b; b = a % b; a = t; }
    a
}
```
Add to `exports()`:
```rust
("sign", bi("math.sign")), ("trunc", bi("math.trunc")), ("clamp", bi("math.clamp")),
("hypot", bi("math.hypot")), ("gcd", bi("math.gcd")), ("lcm", bi("math.lcm")),
```
Add arms in `call`:
```rust
"sign" => {
    let x = want_number(&arg(args, 0), span, &ctx("sign"))?;
    let r = if x.is_nan() { f64::NAN } else if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else { 0.0 };
    Ok(Value::Number(r))
}
"trunc" => Ok(Value::Number(want_number(&arg(args, 0), span, &ctx("trunc"))?.trunc())),
"clamp" => {
    let x = want_number(&arg(args, 0), span, &ctx("clamp"))?;
    let lo = want_number(&arg(args, 1), span, &ctx("clamp"))?;
    let hi = want_number(&arg(args, 2), span, &ctx("clamp"))?;
    if lo > hi {
        return Err(AsError::at("math.clamp requires lo <= hi", span).into());
    }
    Ok(Value::Number(x.max(lo).min(hi)))
}
"hypot" => {
    let x = want_number(&arg(args, 0), span, &ctx("hypot"))?;
    let y = want_number(&arg(args, 1), span, &ctx("hypot"))?;
    Ok(Value::Number(x.hypot(y)))
}
"gcd" => {
    let a = want_int(want_number(&arg(args, 0), span, &ctx("gcd"))?, span, "math.gcd")?;
    let b = want_int(want_number(&arg(args, 1), span, &ctx("gcd"))?, span, "math.gcd")?;
    Ok(Value::Number(gcd_i64(a, b) as f64))
}
"lcm" => {
    let a = want_int(want_number(&arg(args, 0), span, &ctx("lcm"))?, span, "math.lcm")?;
    let b = want_int(want_number(&arg(args, 1), span, &ctx("lcm"))?, span, "math.lcm")?;
    let g = gcd_i64(a, b);
    let r = if g == 0 { 0 } else { (a / g * b).abs() };
    Ok(Value::Number(r as f64))
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib stdlib::math::tests::math_scalar_helpers`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/stdlib/math.rs
git commit -m "feat(math): sign/trunc/clamp/hypot/gcd/lcm"
```

---

## Task 8: `math` array statistics (sum/mean/median/variance/stddev)

**Files:**
- Modify: `src/stdlib/math.rs` (exports + arms + tests + free helper)

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn math_stats() {
    let a = Value::Array(std::rc::Rc::new(std::cell::RefCell::new(vec![n(1.0), n(2.0), n(3.0), n(4.0)])));
    assert_eq!(call("sum", &[a.clone()], sp()).unwrap(), n(10.0));
    assert_eq!(call("mean", &[a.clone()], sp()).unwrap(), n(2.5));
    assert_eq!(call("median", &[a.clone()], sp()).unwrap(), n(2.5));
    // population variance of [1,2,3,4] = 1.25
    assert_eq!(call("variance", &[a.clone()], sp()).unwrap(), n(1.25));
    // sample variance = 5/3
    let sv = call("variance", &[a.clone(), Value::Bool(true)], sp()).unwrap();
    assert!(matches!(sv, Value::Number(x) if (x - 5.0/3.0).abs() < 1e-12));
    let empty = Value::Array(std::rc::Rc::new(std::cell::RefCell::new(vec![])));
    assert_eq!(call("sum", &[empty.clone()], sp()).unwrap(), n(0.0));
    assert!(matches!(call("mean", &[empty], sp()), Err(Control::Panic(_))));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib stdlib::math::tests::math_stats`
Expected: FAIL.

- [ ] **Step 3: Implement**

Add a free helper at module scope (reuse `want_array`/`want_number` — add `use super::want_array;` to the `math.rs` imports if not present):
```rust
/// Collect an array argument into a Vec<f64>, panicking on non-number elements.
fn want_number_vec(v: &Value, span: Span, ctx: &str) -> Result<Vec<f64>, Control> {
    let a = want_array(v, span, ctx)?;
    let mut out = Vec::with_capacity(a.borrow().len());
    for el in a.borrow().iter() {
        out.push(want_number(el, span, ctx)?);
    }
    Ok(out)
}
```
Add to `exports()`:
```rust
("sum", bi("math.sum")), ("mean", bi("math.mean")), ("median", bi("math.median")),
("variance", bi("math.variance")), ("stddev", bi("math.stddev")),
```
Add arms in `call`:
```rust
"sum" => {
    let xs = want_number_vec(&arg(args, 0), span, &ctx("sum"))?;
    Ok(Value::Number(xs.iter().sum()))
}
"mean" => {
    let xs = want_number_vec(&arg(args, 0), span, &ctx("mean"))?;
    if xs.is_empty() {
        return Err(AsError::at("math.mean of empty array", span).into());
    }
    Ok(Value::Number(xs.iter().sum::<f64>() / xs.len() as f64))
}
"median" => {
    let mut xs = want_number_vec(&arg(args, 0), span, &ctx("median"))?;
    if xs.is_empty() {
        return Err(AsError::at("math.median of empty array", span).into());
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let m = xs.len() / 2;
    let med = if xs.len() % 2 == 1 { xs[m] } else { (xs[m - 1] + xs[m]) / 2.0 };
    Ok(Value::Number(med))
}
"variance" | "stddev" => {
    let xs = want_number_vec(&arg(args, 0), span, &ctx(func))?;
    let sample = matches!(args.get(1), Some(v) if v.is_truthy());
    if xs.is_empty() {
        return Err(AsError::at(format!("math.{} of empty array", func), span).into());
    }
    if sample && xs.len() < 2 {
        return Err(AsError::at(format!("math.{} (sample) requires at least 2 values", func), span).into());
    }
    let mean = xs.iter().sum::<f64>() / xs.len() as f64;
    let ss: f64 = xs.iter().map(|x| (x - mean).powi(2)).sum();
    let denom = if sample { xs.len() as f64 - 1.0 } else { xs.len() as f64 };
    let var = ss / denom;
    Ok(Value::Number(if func == "stddev" { var.sqrt() } else { var }))
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib stdlib::math::tests::math_stats`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/stdlib/math.rs
git commit -m "feat(math): sum/mean/median/variance/stddev (sample flag)"
```

---

## Task 9: `math` random (randomInt/shuffle/choice)

**Files:**
- Modify: `src/stdlib/math.rs` (exports + arms + tests). Reuses the existing `next_random()`.

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn math_random_helpers() {
    for _ in 0..100 {
        let r = call("randomInt", &[n(1.0), n(6.0)], sp()).unwrap();
        if let Value::Number(x) = r { assert!((1.0..=6.0).contains(&x) && x.fract() == 0.0); } else { panic!() }
    }
    // min == max always returns that value
    assert_eq!(call("randomInt", &[n(5.0), n(5.0)], sp()).unwrap(), n(5.0));
    // min > max panics
    assert!(matches!(call("randomInt", &[n(6.0), n(1.0)], sp()), Err(Control::Panic(_))));
    let a = Value::Array(std::rc::Rc::new(std::cell::RefCell::new(vec![n(1.0), n(2.0), n(3.0)])));
    let sh = call("shuffle", &[a.clone()], sp()).unwrap();
    if let Value::Array(v) = sh { assert_eq!(v.borrow().len(), 3); } else { panic!() }
    let empty = Value::Array(std::rc::Rc::new(std::cell::RefCell::new(vec![])));
    assert_eq!(call("choice", &[empty], sp()).unwrap(), Value::Nil);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib stdlib::math::tests::math_random_helpers`
Expected: FAIL.

- [ ] **Step 3: Implement**

Add to `exports()`:
```rust
("randomInt", bi("math.randomInt")), ("shuffle", bi("math.shuffle")), ("choice", bi("math.choice")),
```
Add arms in `call` (note: `next_random()` returns f64 in `[0, 1)`; ensure `use std::rc::Rc; use std::cell::RefCell;` are imported in `math.rs`):
```rust
"randomInt" => {
    let min = want_int(want_number(&arg(args, 0), span, &ctx("randomInt"))?, span, "math.randomInt")?;
    let max = want_int(want_number(&arg(args, 1), span, &ctx("randomInt"))?, span, "math.randomInt")?;
    if min > max {
        return Err(AsError::at("math.randomInt requires min <= max", span).into());
    }
    let span_len = (max - min + 1) as f64;
    let v = min + (next_random() * span_len).floor() as i64;
    Ok(Value::Number(v as f64))
}
"shuffle" => {
    let a = want_array(&arg(args, 0), span, &ctx("shuffle"))?;
    let mut items = a.borrow().clone();
    // Fisher–Yates
    let len = items.len();
    for i in (1..len).rev() {
        let j = (next_random() * (i as f64 + 1.0)).floor() as usize;
        items.swap(i, j);
    }
    Ok(Value::Array(Rc::new(RefCell::new(items))))
}
"choice" => {
    let a = want_array(&arg(args, 0), span, &ctx("choice"))?;
    let b = a.borrow();
    if b.is_empty() {
        return Ok(Value::Nil);
    }
    let idx = (next_random() * b.len() as f64).floor() as usize;
    Ok(b[idx.min(b.len() - 1)].clone())
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib stdlib::math::tests::math_random_helpers`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/stdlib/math.rs
git commit -m "feat(math): randomInt(inclusive)/shuffle/choice"
```

---

## Task 10: `object` pure fns (fromEntries/pick/omit/deepClone/deepEqual)

**Files:**
- Modify: `src/stdlib/object.rs` (exports + arms + free helpers + tests)

- [ ] **Step 1: Write failing tests**

Add a `tests` module to `object.rs` (or extend an existing one):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    fn sp() -> Span { Span::new(0, 0) }
    fn s(x: &str) -> Value { Value::Str(x.into()) }
    fn obj(pairs: Vec<(&str, Value)>) -> Value {
        let mut m = IndexMap::new();
        for (k, v) in pairs { m.insert(k.to_string(), v); }
        Value::Object(Rc::new(RefCell::new(m)))
    }

    #[test]
    fn object_pure() {
        let o = obj(vec![("a", Value::Number(1.0)), ("b", Value::Number(2.0)), ("c", Value::Number(3.0))]);
        let keys = arr(vec![s("a"), s("c")]);
        assert_eq!(call("pick", &[o.clone(), keys.clone()], sp()).unwrap().to_string(), obj(vec![("a", Value::Number(1.0)), ("c", Value::Number(3.0))]).to_string());
        assert_eq!(call("omit", &[o.clone(), keys], sp()).unwrap().to_string(), obj(vec![("b", Value::Number(2.0))]).to_string());
        let entries = arr(vec![arr(vec![s("x"), Value::Number(9.0)])]);
        assert_eq!(call("fromEntries", &[entries], sp()).unwrap().to_string(), obj(vec![("x", Value::Number(9.0))]).to_string());
        // deepEqual: two distinct-but-equal objects
        assert_eq!(call("deepEqual", &[o.clone(), obj(vec![("a", Value::Number(1.0)), ("b", Value::Number(2.0)), ("c", Value::Number(3.0))])], sp()).unwrap(), Value::Bool(true));
        // deepClone makes an independent copy
        let cloned = call("deepClone", &[o.clone()], sp()).unwrap();
        assert_eq!(call("deepEqual", &[o, cloned], sp()).unwrap(), Value::Bool(true));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib stdlib::object`
Expected: FAIL.

- [ ] **Step 3: Implement**

Add imports to `object.rs`:
```rust
use crate::value::{Instance, MapKey};
use std::collections::HashMap;
```
Add free helpers at module scope:
```rust
/// Structural deep equality (distinct from `==`, which is identity for containers).
pub(crate) fn deep_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| deep_equal(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| deep_equal(v, w)))
        }
        (Value::Map(x), Value::Map(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| deep_equal(v, w)))
        }
        (Value::Bytes(x), Value::Bytes(y)) => *x.borrow() == *y.borrow(),
        (Value::Instance(x), Value::Instance(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            Rc::ptr_eq(&x.class, &y.class)
                && x.fields.len() == y.fields.len()
                && x.fields.iter().all(|(k, v)| y.fields.get(k).is_some_and(|w| deep_equal(v, w)))
        }
        // primitives and identity-compared kinds fall back to ==
        _ => a == b,
    }
}

/// Deep copy of containers; shares functions/natives/etc. Cycle- and sharing-safe.
pub(crate) fn deep_clone(v: &Value, seen: &mut HashMap<usize, Value>) -> Value {
    match v {
        Value::Array(rc) => {
            let key = Rc::as_ptr(rc) as usize;
            if let Some(c) = seen.get(&key) { return c.clone(); }
            let out = Rc::new(RefCell::new(Vec::new()));
            let cloned = Value::Array(out.clone());
            seen.insert(key, cloned.clone());
            let src = rc.borrow().clone();
            let mut dst = out.borrow_mut();
            for el in src.iter() { dst.push(deep_clone(el, seen)); }
            cloned
        }
        Value::Object(rc) => {
            let key = Rc::as_ptr(rc) as usize;
            if let Some(c) = seen.get(&key) { return c.clone(); }
            let out = Rc::new(RefCell::new(IndexMap::new()));
            let cloned = Value::Object(out.clone());
            seen.insert(key, cloned.clone());
            let src = rc.borrow().clone();
            let mut dst = out.borrow_mut();
            for (k, val) in src.iter() { dst.insert(k.clone(), deep_clone(val, seen)); }
            cloned
        }
        Value::Map(rc) => {
            let key = Rc::as_ptr(rc) as usize;
            if let Some(c) = seen.get(&key) { return c.clone(); }
            let out = Rc::new(RefCell::new(IndexMap::<MapKey, Value>::new()));
            let cloned = Value::Map(out.clone());
            seen.insert(key, cloned.clone());
            let src = rc.borrow().clone();
            let mut dst = out.borrow_mut();
            for (k, val) in src.iter() { dst.insert(k.clone(), deep_clone(val, seen)); }
            cloned
        }
        Value::Bytes(rc) => Value::Bytes(Rc::new(RefCell::new(rc.borrow().clone()))),
        Value::Instance(rc) => {
            let key = Rc::as_ptr(rc) as usize;
            if let Some(c) = seen.get(&key) { return c.clone(); }
            let src = rc.borrow();
            let out = Rc::new(RefCell::new(Instance { class: src.class.clone(), fields: IndexMap::new() }));
            let cloned = Value::Instance(out.clone());
            seen.insert(key, cloned.clone());
            let fields = src.fields.clone();
            drop(src);
            let mut dst = out.borrow_mut();
            for (k, val) in fields.iter() { dst.fields.insert(k.clone(), deep_clone(val, seen)); }
            cloned
        }
        other => other.clone(),
    }
}
```
> The implementer must confirm `Value::Instance` is `Rc<RefCell<Instance>>` and `Instance { class, fields }` match `src/value.rs`. If `Bytes` is not `Rc<RefCell<Vec<u8>>>`, adjust the `Bytes` arms.

Add to `exports()`:
```rust
("fromEntries", bi("object.fromEntries")),
("pick", bi("object.pick")),
("omit", bi("object.omit")),
("mapValues", bi("object.mapValues")),
("deepClone", bi("object.deepClone")),
("deepEqual", bi("object.deepEqual")),
```
Add arms in `call`:
```rust
"fromEntries" => {
    let pairs = want_array(&arg(args, 0), span, &ctx("fromEntries"))?;
    let mut out = IndexMap::new();
    for pair in pairs.borrow().iter() {
        let p = want_array(pair, span, &ctx("fromEntries"))?;
        let p = p.borrow();
        let k = want_string(&p.first().cloned().unwrap_or(Value::Nil), span, &ctx("fromEntries"))?;
        let v = p.get(1).cloned().unwrap_or(Value::Nil);
        out.insert(k.to_string(), v);
    }
    Ok(Value::Object(Rc::new(RefCell::new(out))))
}
"pick" => {
    let o = want_object(&arg(args, 0), span, &ctx("pick"))?;
    let keys = want_array(&arg(args, 1), span, &ctx("pick"))?;
    let src = o.borrow();
    let mut out = IndexMap::new();
    for k in keys.borrow().iter() {
        let k = want_string(k, span, &ctx("pick"))?;
        if let Some(v) = src.get(k.as_ref()) {
            out.insert(k.to_string(), v.clone());
        }
    }
    Ok(Value::Object(Rc::new(RefCell::new(out))))
}
"omit" => {
    let o = want_object(&arg(args, 0), span, &ctx("omit"))?;
    let keys = want_array(&arg(args, 1), span, &ctx("omit"))?;
    let drop_set: std::collections::HashSet<String> = keys
        .borrow().iter()
        .map(|k| want_string(k, span, &ctx("omit")).map(|s| s.to_string()))
        .collect::<Result<_, _>>()?;
    let out: IndexMap<String, Value> = o.borrow().iter()
        .filter(|(k, _)| !drop_set.contains(*k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Ok(Value::Object(Rc::new(RefCell::new(out))))
}
"deepEqual" => Ok(Value::Bool(deep_equal(&arg(args, 0), &arg(args, 1)))),
"deepClone" => {
    let mut seen = HashMap::new();
    Ok(deep_clone(&arg(args, 0), &mut seen))
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib stdlib::object`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/stdlib/object.rs
git commit -m "feat(object): fromEntries/pick/omit/deepClone(cycle-safe)/deepEqual"
```

---

## Task 11: `object.mapValues` (callback) + routing

**Files:**
- Modify: `src/stdlib/object.rs` (add `impl Interp::call_object`)
- Modify: `src/stdlib/mod.rs:171` (route `object` through `call_object`)

- [ ] **Step 1: Write a failing `.as`-driven test stub**

`mapValues` takes a callback, so it is covered by the Task 12 example. For a unit-level guard, add to `object.rs` tests:
```rust
#[tokio::test]
async fn map_values_doubles() {
    let interp = Interp::new();
    let o = obj(vec![("a", Value::Number(1.0)), ("b", Value::Number(2.0))]);
    // identity callback via a no-op: build a Value::Function is awkward; assert routing exists instead.
    // Calling with a non-function callback must Tier-2 panic (not "unknown function").
    let r = interp.call_object("mapValues", &[o, Value::Number(0.0)], sp()).await;
    assert!(matches!(r, Err(Control::Panic(_))));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib stdlib::object::tests::map_values_doubles`
Expected: FAIL (`call_object` does not exist).

- [ ] **Step 3: Implement**

Add imports to `object.rs`:
```rust
use crate::interp::Interp;
```
Add an `impl Interp` block in `object.rs`:
```rust
impl Interp {
    /// Object dispatch: callback-taking fns here; everything else delegates to the
    /// pure `object::call`.
    pub(crate) async fn call_object(&self, func: &str, args: &[Value], span: Span) -> Result<Value, Control> {
        match func {
            "mapValues" => {
                let o = want_object(&arg(args, 0), span, "object.mapValues")?;
                let f = arg(args, 1);
                let src = o.borrow().clone();
                let mut out = IndexMap::new();
                for (k, v) in src.iter() {
                    let mapped = self
                        .call_value(f.clone(), vec![v.clone(), Value::Str(k.as_str().into())], span)
                        .await?;
                    out.insert(k.clone(), mapped);
                }
                Ok(Value::Object(Rc::new(RefCell::new(out))))
            }
            _ => call(func, args, span),
        }
    }
}
```
In `src/stdlib/mod.rs`, change line `:171`:
```rust
"object" => self.call_object(func, args, span).await,
```
(`call_value` on a non-function value already produces a Tier-2 panic, satisfying the test.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib stdlib::object && cargo test --lib stdlib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/stdlib/object.rs src/stdlib/mod.rs
git commit -m "feat(object): mapValues callback via call_object dispatch"
```

---

## Task 12: Checksums in `crypto` (crc32 + xxhash)

**Files:**
- Modify: `Cargo.toml` (add `crc32fast`, `xxhash-rust` to the `crypto` feature)
- Modify: `src/stdlib/crypto.rs` (exports + arms + tests)

- [ ] **Step 1: Add dependencies**

In `Cargo.toml`, extend the `crypto` feature list and add the deps:
```toml
crypto = ["dep:sha2", "dep:md-5", "dep:hmac", "dep:argon2", "dep:bcrypt", "dep:rand", "dep:hex", "dep:crc32fast", "dep:xxhash-rust"]
```
```toml
crc32fast = { version = "1", optional = true }
xxhash-rust = { version = "0.8", features = ["xxh64"], optional = true }
```

- [ ] **Step 2: Write failing tests**

Add to `crypto.rs` tests:
```rust
#[test]
fn checksums() {
    // crc32 of "hello" (IEEE) = 0x3610A686 = 907060870
    assert_eq!(call("crc32", &[Value::Str("hello".into())], sp()).unwrap(), Value::Number(907060870.0));
    // xxhash returns a lowercase hex string of fixed width (16 chars for xxh64)
    let r = call("xxhash", &[Value::Str("hello".into())], sp()).unwrap();
    if let Value::Str(s) = r { assert_eq!(s.len(), 16); assert!(s.chars().all(|c| c.is_ascii_hexdigit())); } else { panic!() }
}
```
(Match the existing `crypto.rs` test helpers; add `fn sp()` if absent.)

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --lib stdlib::crypto::tests::checksums`
Expected: FAIL.

- [ ] **Step 4: Implement**

Add to `exports()` in `crypto.rs`:
```rust
("crc32", bi("crypto.crc32")),
("xxhash", bi("crypto.xxhash")),
```
Add a helper to accept bytes-or-string (mirror however existing hashes read input; if they use `want_string`/`want_bytes`, follow that). Then add arms in `call`:
```rust
"crc32" => {
    let bytes = checksum_input(&arg(args, 0), span, "crypto.crc32")?;
    let mut h = crc32fast::Hasher::new();
    h.update(&bytes);
    Ok(Value::Number(h.finalize() as f64))
}
"xxhash" => {
    let bytes = checksum_input(&arg(args, 0), span, "crypto.xxhash")?;
    let digest = xxhash_rust::xxh64::xxh64(&bytes, 0);
    Ok(Value::Str(format!("{:016x}", digest).into()))
}
```
Add the input helper at module scope (string → UTF-8 bytes; bytes → as-is):
```rust
fn checksum_input(v: &Value, span: Span, ctx: &str) -> Result<Vec<u8>, Control> {
    match v {
        Value::Str(s) => Ok(s.as_bytes().to_vec()),
        Value::Bytes(b) => Ok(b.borrow().clone()),
        _ => Err(crate::error::AsError::at(format!("{} expects a string or bytes", ctx), span).into()),
    }
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --lib stdlib::crypto::tests::checksums`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/stdlib/crypto.rs
git commit -m "feat(crypto): crc32 (number) + xxhash (hex string) checksums"
```

---

## Task 13: Example, docs, and full verification

**Files:**
- Create: `examples/stdlib_completeness.as`
- Modify: `docs/content/stdlib/collections.md` (array/string/object new fns)
- Modify: math + crypto stdlib doc pages
- Modify: `README.md` if it lists function-level stdlib entries

- [ ] **Step 1: Write the runnable example (also exercises callback fns end-to-end)**

Create `examples/stdlib_completeness.as`:
```
// array
let nums = [3, 1, 2, 1, 4]
assert(array.find(nums, fn(x) { return x > 2 }) == 3, "find")
assert(array.findIndex(nums, fn(x) { return x == 2 }) == 2, "findIndex")
assert(array.some(nums, fn(x) { return x == 4 }), "some")
assert(array.every(nums, fn(x) { return x > 0 }), "every")
assert(array.every([], fn(x) { return false }), "every empty is true")
assert(array.indexOf(nums, 4) == 4, "indexOf")
assert(len(array.unique(nums)) == 4, "unique")
assert(array.flat([[1], [2, 3]])[2] == 3, "flat")
assert(array.flatMap([1, 2], fn(x) { return [x, x] })[3] == 2, "flatMap")
assert(array.first(nums) == 3 && array.last(nums) == 4, "first/last")
assert(len(array.chunk(nums, 2)) == 3, "chunk")
let pairs = array.zip([1, 2], ["a", "b"])
assert(pairs[0][1] == "a", "zip")
let parts = array.partition(nums, fn(x) { return x > 1 })
assert(len(parts[0]) == 3 && len(parts[1]) == 2, "partition")
let groups = array.groupBy(nums, fn(x) { return x % 2 })
assert(len(groups.get(1)) == 3, "groupBy -> Map")

// string
assert(string.startsWith("hello", "he"), "startsWith")
assert(string.replace("a.b.c", ".", "-") == "a-b.c", "replace first")
assert(string.replaceAll("a.b.c", ".", "-") == "a-b-c", "replaceAll")
assert(string.reverse("abc") == "cba", "reverse")
assert(len(string.lines("a\nb")) == 2, "lines")

// math
assert(math.gcd(12, 8) == 4, "gcd")
assert(math.clamp(5, 0, 3) == 3, "clamp")
assert(math.sum([1, 2, 3, 4]) == 10, "sum")
assert(math.mean([1, 2, 3, 4]) == 2.5, "mean")
assert(math.min(...[3, 1, 2]) == 1, "min via spread")
let r = math.randomInt(1, 1)
assert(r == 1, "randomInt inclusive")

// object
let o = { a: 1, b: 2, c: 3 }
assert(object.deepEqual(o, { a: 1, b: 2, c: 3 }), "deepEqual")
let doubled = object.mapValues(o, fn(v, k) { return v * 2 })
assert(doubled.b == 4, "mapValues")
let picked = object.pick(o, ["a", "c"])
assert(picked.a == 1 && object.has(picked, "b") == false, "pick")

// crypto checksums
assert(crypto.crc32("hello") == 907060870, "crc32")
assert(len(crypto.xxhash("hello")) == 16, "xxhash hex")

print("stdlib completeness: all assertions passed")
```

- [ ] **Step 2: Run the example**

Run: `cargo run -- run examples/stdlib_completeness.as`
Expected: prints `stdlib completeness: all assertions passed`.

- [ ] **Step 3: Register the example with the conformance tests if needed**

Check `tests/cli.rs` / `tests/treesitter_conformance.rs` — if examples are auto-discovered from `examples/*.as`, nothing to do; otherwise add `stdlib_completeness.as` to the executed list. Run: `cargo test --test cli` and `cargo test --test treesitter_conformance`. Expected: PASS.

- [ ] **Step 4: Update docs**

- `docs/content/stdlib/collections.md`: document the new `array`, `string`, and `object` functions (signatures + one-line behavior); the `replace`/`replaceAll` example was already fixed in Task 1.
- The math stdlib page: trig/log/exp, sign/trunc/clamp/hypot/gcd/lcm, sum/mean/median/variance/stddev (note the `sample` flag), randomInt (inclusive)/shuffle/choice.
- The crypto stdlib page: `crc32` (returns a number), `xxhash` (returns a 16-char hex string).
- `README.md`: update the stdlib table if it enumerates functions.

- [ ] **Step 5: Full verification (both feature configs)**

Run and confirm all green:
```bash
cargo test
cargo test --no-default-features
cargo clippy --all-targets
cargo clippy --no-default-features --all-targets
cargo fmt
```
Expected: all tests pass; clippy clean in both configs.

- [ ] **Step 6: Commit**

```bash
git add examples/stdlib_completeness.as docs/ README.md tests/
git commit -m "docs+example(phase1): stdlib completeness example, docs, conformance"
```

---

## Self-review notes (author)

- **Spec coverage:** every spec function maps to a task — array (T3/T4/T5), string (T1/T2), math (T6/T7/T8/T9), object (T10/T11), checksums (T12), `string.replace` breaking change (T1), docs/example (T13). `object.freeze` is intentionally **not** present (deferred per spec).
- **Type consistency:** `Value::Number`/`Value::Str`/`Value::Bool`/`Value::Array`/`Value::Object`/`Value::Map` used consistently; `groupBy` returns `Value::Map`; `xxhash` returns `Value::Str`; `crc32` returns `Value::Number` — matches the spec decisions.
- **Known follow-ups for the implementer:** confirm `Value::Instance`/`Value::Bytes` shapes in `deep_clone`/`deep_equal` (Task 10) against `src/value.rs`; confirm `crypto.rs` input-reading style for `checksum_input` (Task 12); confirm whether `examples/*.as` are auto-discovered by conformance tests (Task 13 Step 3).
