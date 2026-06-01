fn classify(n: number): string {
  return match n { _ if n < 0 => "negative", 0 => "zero", 1..=9 => "single digit", 10..100 => "double digit", _ => "big" }
}
print(classify(-3))
print(classify(0))
print(classify(7))
print(classify(42))
print(classify(500))
fn describe(xs: array<number>): string {
  return match xs { [] => "empty", [x] => `one: ${x}`, [first, ...rest] => `head ${first}, ${len(rest)} more` }
}
print(describe([]))
print(describe([9]))
print(describe([1, 2, 3]))
fn unwrapPair(pair: array<any>): string {
  return match pair { [u, nil] => `ok: ${u}`, [_, e] => `err: ${e}`, _ => "shape?" }
}
print(unwrapPair([42, nil]))
print(unwrapPair([nil, "boom"]))
fn route(req: object): string {
  return match req { {method, path} => `${method} ${path}`, _ => "?" }
}
print(route({ method: "GET", path: "/users" }))
fn role(user: object): string {
  return match user { {role: "admin"} => "is admin", {role: r, ...rest} => `role ${r}`, _ => "no role" }
}
print(role({ role: "admin" }))
print(role({ role: "guest", name: "Sam" }))
fn weekend(day: string): bool {
  return match day { "sat" | "sun" => true, other => false }
}
print(weekend("sat"))
print(weekend("mon"))
