fn safeDivide(a, b) {
  if (b == 0) { return Err("division by zero") }
  return Ok(a / b)
}

fn compute(a, b, c) {
  let x = safeDivide(a, b)?
  let y = safeDivide(x, c)?
  return Ok(y)
}

let good = compute(100, 5, 2)
print(good[0])

let bad = compute(100, 0, 2)
print(bad[0])
print(bad[1].message)

fn willPanic() {
  let arr = [1, 2]
  return arr[99]
}
let recovered = recover(willPanic)
print(recovered[0])
print(recovered[1].message)
