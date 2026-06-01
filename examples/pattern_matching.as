// Pattern matching (Phase 8) — array / object / range patterns, bare-ident
// bindings (Option C), `|` alternatives, and `pattern if <guard>` guards.

// --- Range patterns: `a..b` (exclusive) and `a..=b` (inclusive) ---
fn classify(n: number): string {
    return match n {
        _ if n < 0 => "negative",
        0 => "zero",
        1..=9 => "single digit",
        10..100 => "double digit",
        _ => "big",
    }
}

print(classify(-3))   // negative
print(classify(0))    // zero
print(classify(7))    // single digit
print(classify(42))   // double digit
print(classify(500))  // big

// --- Array patterns: fixed arity, rest, and nested value compare ---
fn describe(xs: array<number>): string {
    return match xs {
        [] => "empty",
        [x] => `one: ${x}`,
        [first, ...rest] => `head ${first}, ${len(rest)} more`,
    }
}

print(describe([]))         // empty
print(describe([9]))        // one: 9
print(describe([1, 2, 3]))  // head 1, 2 more

// `[u, nil]` — a value pattern (nil) mixed with a binding.
fn unwrapPair(pair: array<any>): string {
    return match pair {
        [u, nil] => `ok: ${u}`,
        [_, e] => `err: ${e}`,
        _ => "shape?",
    }
}

print(unwrapPair([42, nil]))      // ok: 42
print(unwrapPair([nil, "boom"]))  // err: boom

// --- Object patterns: shorthand binds, sub-patterns, rest ---
fn route(req: object): string {
    return match req {
        {method, path} => `${method} ${path}`,
        _ => "?",
    }
}

print(route({ method: "GET", path: "/users" }))  // GET /users

fn role(user: object): string {
    return match user {
        {role: "admin"} => "is admin",
        {role: r, ...rest} => `role ${r}`,
        _ => "no role",
    }
}

print(role({ role: "admin" }))                 // is admin
print(role({ role: "guest", name: "Sam" }))    // role guest

// --- Or-patterns + bare-ident binding fall-through ---
fn weekend(day: string): bool {
    return match day {
        "sat" | "sun" => true,
        other => false,
    }
}

print(weekend("sat"))  // true
print(weekend("mon"))  // false
