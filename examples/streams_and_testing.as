import * as stream from "std/stream"
import * as assert from "std/assert"
import * as bench from "std/bench"
let big = stream.range(0, 1000000)
let evens = stream.filter(big, x => x % 2 == 0)
let tripled = stream.map(evens, x => x * 3)
let first5 = stream.take(tripled, 5)
let lazy_result = await stream.collect(first5)
assert.eq(lazy_result, [0, 6, 12, 18, 24], "lazy 1M-range pipeline")
let nums = stream.from([10, 3, 7, 1, 8, 4])
let total = await stream.reduce(nums, (acc, x) => acc + x, 0)
assert.eq(total, 33, "reduce sum")
let big_nums = stream.filter(stream.from([10, 3, 7, 1, 8, 4]), x => x > 5)
let n = await stream.count(big_nums)
assert.eq(n, 3, "count > 5")
let found = await stream.find(stream.from([1, 2, 3, 4, 5]), x => x > 3)
assert.eq(found, 4, "find first > 3")
let after_drop = await stream.collect(stream.drop(stream.from([1, 2, 3, 4, 5]), 2))
assert.eq(after_drop, [3, 4, 5], "drop(2)")
let enumerated = await stream.collect(stream.enumerate(stream.from(["a", "b", "c"])))
assert.eq(enumerated, [[0, "a"], [1, "b"], [2, "c"]], "enumerate")
let flat = await stream.collect(stream.flatMap(stream.from([1, 2, 3]), x => [x, x * 10]))
assert.eq(flat, [1, 10, 2, 20, 3, 30], "flatMap")
let zipped = await stream.collect(stream.zip(stream.from([1, 2, 3]), stream.from(["a", "b"])))
assert.eq(zipped, [[1, "a"], [2, "b"]], "zip ends at shorter")
let once = stream.from([100, 200, 300])
let first_run = await stream.collect(once)
let second_run = await stream.collect(once)
assert.eq(first_run, [100, 200, 300], "first consumption")
assert.eq(second_run, [], "second consumption is empty (single-use)")
test("lazy 1M-range pipeline is bounded by take(5)", () => {
  let calls = [0]
  let s = stream.take(stream.map(stream.filter(stream.range(0, 1000000), x => x % 2 == 0), x => {
  calls[0] = calls[0] + 1
  return x * 3
}), 5)
  let res = await stream.collect(s)
  assert.eq(res, [0, 6, 12, 18, 24])
  assert.lte(calls[0], 5)
})
test("assert.eq deep equality", () => {
  assert.eq([1, [2, 3]], [1, [2, 3]])
  assert.eq({ a: 1, b: { c: 2 } }, { a: 1, b: { c: 2 } })
})
test("assert.isTrue / isFalse / isNil / notNil", () => {
  assert.isTrue(1 > 0)
  assert.isFalse(1 > 2)
  assert.isNil(nil)
  assert.notNil(42)
})
test("assert.contains — string, array, object", () => {
  assert.contains("hello world", "world")
  assert.contains([1, 2, 3], 2)
  assert.contains({ key: "val" }, "key")
})
test("assert.approxEq for floating-point", () => {
  assert.approxEq(0.1 + 0.2, 0.3)
  assert.approxEq(1, 1.09, 0.1)
})
test("assert.throws captures the panic", () => {
  let e = assert.throws(() => assert.eq(1, 99))
  assert.contains(e.message, "assert.eq failed")
})
test("assert.gt / gte / lt / lte", () => {
  assert.gt(10, 5)
  assert.gte(5, 5)
  assert.lt(3, 7)
  assert.lte(7, 7)
})
test("stream.reduce, count, find", () => {
  let s1 = stream.from([1, 2, 3, 4, 5])
  assert.eq(await stream.reduce(s1, (a, b) => a + b, 0), 15)
  let s2 = stream.filter(stream.from([1, 2, 3, 4, 5]), x => x % 2 == 0)
  assert.eq(await stream.count(s2), 2)
  let s3 = stream.from([10, 20, 30])
  assert.eq(await stream.first(s3), 10)
})
test("stream single consumption", () => {
  let s = stream.from([7, 8, 9])
  assert.eq(await stream.collect(s), [7, 8, 9])
  assert.eq(await stream.collect(s), [])
})
let stats = await bench.measure(() => {
  let acc = 0
  let i = 0
  while (i < 50) {
    acc = acc + i
    i = i + 1
  }
  return acc
}, 100)
assert.eq(stats.iterations, 100, "bench iterations matches")
assert.isTrue(stats.totalMs >= 0, "totalMs is non-negative")
assert.isTrue(stats.avgMs >= 0, "avgMs is non-negative")
assert.isTrue(stats.opsPerSec > 0, "opsPerSec is positive")
let cmp = await bench.compare({ sum_loop: () => {
  let x = 0
  let i = 0
  while (i < 20) {
    x = x + i
    i = i + 1
  }
  return x
}, identity: () => 42 }, 50)
assert.isTrue(len(cmp) == 2, "compare returns two entries")
assert.notNil(cmp[0].name, "first entry has a name")
assert.isTrue(cmp[0].opsPerSec > 0, "first entry opsPerSec is positive")
print("streams and testing: all assertions passed")
