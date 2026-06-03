// all_features.as — a deterministic showcase exercising most of the language.
// Runs byte-identically on the tree-walker and the bytecode VM (and as a built

// .aso), parses on every front-end, and prints labeled output per section. The

// output is fully deterministic; the final line is `all_features ok`.
import * as math from "std/math"
import * as arr from "std/array"
import * as json from "std/json"
import * as task from "std/task"

// ── values & bindings ──────────────────────────────────────────────────────
let n = 42
const PI_ISH = 3.5
let s = "hello"
let flag = true
let nothing = nil
let xs = [1, 2, 3]
let obj = {a: 1, b: 2}
print("values:", n, PI_ISH, s, flag, nothing)
print("array:", xs)
print("object:", obj)

// ── string templates (incl. nested) + escapes ───────────────────────────────
let who = "world"
print(`tmpl: ${s}, ${who}! n=${n} sum=${1 + 2}`)
print(`nested: ${`inner ${who}`} done`)
print("escapes: tab[\t]nl-next\nquote=\"q\" backslash=\\")

// ── functions: regular, arrow, rest, typed contracts, closures ──────────────
fn add(a: number, b: number): number {
  return a + b
}
let square = (x) => x * x
fn greet(name, greeting) {
  return `${greeting}, ${name}`
}
fn sumAll(...nums: array<number>): number {
  let total = 0
  for (v in nums) {
    total = total + v
  }
  return total
}
fn makeCounter() {
  let count = 0
  fn tick() {
    count = count + 1
    return count
  }
  return tick
}
print("add:", add(2, 3))
print("square:", square(5))
print("params:", greet("Ada", "hi"), "/", greet("Ada", "hey"))
print("rest sum:", sumAll(1, 2, 3, 4))
let counter = makeCounter()
print("closure:", counter(), counter(), counter())

// ── forward reference to a LATER top-level const + mutual recursion ──────────

// `useLater` references `LATER_CONST`, declared near the end of the file (module

// globals resolve forward references); it is CALLED below, after that const is set.
fn useLater(): number {
  return LATER_CONST * 2
}
fn isEven(k: number): bool {
  return k == 0 ? true : isOdd(k - 1)
}
fn isOdd(k: number): bool {
  return k == 0 ? false : isEven(k - 1)
}
print("mutual recursion:", isEven(10), isOdd(10))

// ── control flow ─────────────────────────────────────────────────────────────
fn grade(score: number): string {
  if (score >= 90) {
    return "A"
  } else if (score >= 80) {
    return "B"
  } else {
    return "C"
  }
}
print("if/elif/else:", grade(95), grade(85), grade(50))
let i = 0
let acc = 0
while (i < 5) {
  i = i + 1
  if (i == 2) {
    continue
  }
  if (i == 4) {
    break
  }
  acc = acc + i
}
print("while/break/continue acc:", acc)
let rangeSum = 0
for (k in 0..5) {
  rangeSum = rangeSum + k
}
print("for-range sum:", rangeSum)
let joined = ""
for (ch in ["x", "y", "z"]) {
  joined = joined + ch
}
print("for-of:", joined)
print("ternary:", n > 0 ? "pos" : "nonpos")

// ── classes: fields, init, methods, inheritance, super, typed contract ──────
const DEFAULT_ROLE = "guest"
class Animal {
  name: string
  legs: number = 4
  nickname: string?
  role: string = DEFAULT_ROLE
  fn init(name) {
    self.name = name
  }
  fn describe(): string {
    return `${self.name} (${self.legs} legs, ${self.role})`
  }
}
class Dog extends Animal {
  fn init(name) {
    super.init(name)
  }
  fn describe(): string {
    return super.describe() + ", a dog says " + self.sound()
  }
  fn sound(): string {
    return "woof"
  }
}
let a: Animal = Dog("Rex")
print("class:", a.describe())
let validated = Animal.from({name: "Cat", legs: 4})
print("from():", validated.describe(), "/ nickname:", validated.nickname)

// ── enums + matching ─────────────────────────────────────────────────────────
enum Color {
  Red,
  Green,
  Blue,
}
fn colorCode(c: Color): number {
  return match c {
    Color.Red => 1,
    Color.Green => 2,
    _ => 3,
  }
}
print("enum match:", colorCode(Color.Green), colorCode(Color.Blue))

// ── match: literal/range/array/object/guard/Option-C bind-vs-compare ────────
const TARGET = 7
// Option-C: a DEFINED identifier (TARGET) compares; an UNDEFINED one (captured) binds.
fn classify(v): string {
  return match v {
    0 => "zero",
    1..=5 => "small",
    TARGET => "lucky",
    captured if captured > 100 => `big ${captured}`,
    _ => "other",
  }
}
print("match:", classify(0), classify(3), classify(7), classify(500), classify(42))
fn shape(xs): string {
  return match xs {
    [] => "empty",
    [only] => `one:${only}`,
    [head, ...tail] => `head ${head} rest ${len(tail)}`,
  }
}
print("array match:", shape([]), shape([9]), shape([1, 2, 3]))
fn route(req): string {
  return match req {
    { method: "GET", path } => `GET ${path}`,
    { method: m, ...extra } => `${m} (+${len(extra)} fields)`,
    _ => "?",
  }
}
print("object match:", route({method: "GET", path: "/u"}), route({method: "POST", path: "/u", body: 1}))

// ── destructuring ─────────────────────────────────────────────────────────────
let [first, ...others] = [10, 20, 30, 40]
print("array destructure:", first, others)
let {p, q as renamed, ...objRest} = {p: 1, q: 2, r: 3, s: 4}
print("object destructure:", p, renamed, objRest)

// ── spread in literals + call args ──────────────────────────────────────────
let lo = [1, 2]
let hi = [4, 5]
let merged = [...lo, 3, ...hi]
print("array spread:", merged)
let base = {x: 1, y: 2}
let extended = {...base, y: 20, z: 3}
print("object spread:", extended)
print("call spread:", sumAll(...merged))

// ── error handling: Result, ? propagation, ! unwrap, recover ────────────────
fn safeDiv(x, y) {
  if (y == 0) {
    return Err("divide by zero")
  }
  return Ok(x / y)
}
fn chain(x, y, z) {
  let p = safeDiv(x, y)?
  let q = safeDiv(p, z)?
  return Ok(q)
}
let okPair = chain(100, 5, 2)
print("result ok:", okPair[0])
let errPair = chain(100, 0, 2)
print("result err:", errPair[1].message)
print("unwrap !:", safeDiv(10, 2)!)
let recovered = recover(() => safeDiv(1, 0)!)
print("recover:", recovered[1].message)

// ── optional / nullable types that pass contracts ──────────────────────────
fn maybeLen(text: string?): number {
  return text == nil ? -1 : len(text)
}
print("optional type:", maybeLen("abcd"), maybeLen(nil))

// ── generators: fn* consumed via for-await and .next() ──────────────────────
fn* countTo(limit: number) {
  let c = 1
  while (c <= limit) {
    yield c
    c = c + 1
  }
}
let gathered = []
for await (g in countTo(4)) {
  arr.push(gathered, g)
}
print("generator for-await:", gathered)
let gen = countTo(3)
print("generator next:", gen.next(), gen.next(), gen.next())

// ── async: awaited, deterministic (gather a fixed set) ──────────────────────
async fn double(v) {
  return v * 2
}
async fn runAsync() {
  let one = await double(21)
  let many = await task.gather([double(1), double(2), double(3)])
  return [one, many]
}
let asyncOut = await runAsync()
print("async:", asyncOut[0], asyncOut[1])

// ── stdlib usage (deterministic) ────────────────────────────────────────────
print("math:", math.abs(-7), math.max(3, 9), math.floor(3.9))
print("array:", arr.map([1, 2, 3], (v) => v * 10), arr.reduce([1, 2, 3, 4], (acc, v) => acc + v, 0))
print("json:", json.stringify({k: 1, list: [true, nil]})!)

// ── the later const the forward-ref function above depends on ───────────────
const LATER_CONST = 21
print("forward-ref const:", useLater())

print("all_features ok")
