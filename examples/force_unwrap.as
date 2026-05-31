// Postfix `!` force-unwrap (dual of `?`), and its interaction with await/recover.
fn half(n) {
  if (n % 2 != 0) { return Err("odd") }
  return Ok(n / 2)
}

// `!` unwraps a Result pair; on a value pair it yields the value.
assert(half(8)! == 4, "half(8)! == 4")

// On an error pair, `!` panics; `recover` round-trips the message.
let r = recover(() => half(3)!)
assert(r[1] != nil, "half(3)! panics")
assert(r[1].message == "odd", "message preserved")

print("force_unwrap ok")
