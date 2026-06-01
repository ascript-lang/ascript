fn classify(n: number): string {
  return match n { _ if n < 0 => "negative", 0 => "zero", 1..=9 => "single digit", 10..100 => "double digit", _ => "big" }
}
assert(classify(-3) == "negative", "negative")
assert(classify(0) == "zero", "zero")
assert(classify(7) == "single digit", "single digit")
assert(classify(42) == "double digit", "double digit")
assert(classify(500) == "big", "big")
const NOT_FOUND = 404
fn httpLabel(status: number): string {
  return match status { NOT_FOUND => "not found", code if code >= 500 => `server error ${code}`, other => `status ${other}` }
}
assert(httpLabel(404) == "not found", "404")
assert(httpLabel(503) == "server error 503", "503")
assert(httpLabel(200) == "status 200", "200")
fn isWeekend(day: string): bool {
  return match day { "sat" | "sun" => true, _ => false }
}
assert(isWeekend("sat") == true, "saturday is weekend")
assert(isWeekend("sun") == true, "sunday is weekend")
assert(isWeekend("mon") == false, "monday is not weekend")
fn describe(xs: array<number>): string {
  return match xs { [] => "empty", [x] => `one: ${x}`, [first, ...rest] => `head ${first}, ${len(rest)} more` }
}
assert(describe([]) == "empty", "empty array")
assert(describe([9]) == "one: 9", "singleton array")
assert(describe([1, 2, 3]) == "head 1, 2 more", "multi array")
fn unwrapPair(pair: array<any>): string {
  return match pair { [v, nil] => `ok: ${v}`, [_, e] => `err: ${e}`, _ => "unexpected shape" }
}
assert(unwrapPair([42, nil]) == "ok: 42", "ok pair")
assert(unwrapPair([nil, "boom"]) == "err: boom", "err pair")
fn route(req: object): string {
  return match req { {method, path} => `${method} ${path}`, _ => "?" }
}
assert(route({ method: "GET", path: "/users" }) == "GET /users", "route")
fn describeUser(user: object): string {
  return match user { {role: "admin"} => "is admin", {role: r, name: n} => `role ${r}, name ${n}`, {role: r} => `role ${r}`, _ => "no role" }
}
assert(describeUser({ role: "admin" }) == "is admin", "admin")
assert(describeUser({ role: "guest", name: "Sam" }) == "role guest, name Sam", "guest with name")
assert(describeUser({ role: "mod" }) == "role mod", "mod no name")
assert(describeUser({}) == "no role", "no role")
fn summarize(event: object): string {
  return match event { {type: "click", x, y, ...extra} => `click at ${x},${y}`, {type: t, ...extra} => `event ${t}`, _ => "unknown" }
}
assert(summarize({ type: "click", x: 10, y: 20, target: "btn" }) == "click at 10,20", "click event")
assert(summarize({ type: "keydown", key: "Enter" }) == "event keydown", "keydown event")
let score = 87
let grade = match score { 90..=100 => "A", 80..=89 => "B", 70..=79 => "C", _ => "F" }
assert(grade == "B", "grade B")
print("All pattern matching assertions passed.")
