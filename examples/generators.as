import * as time from "std/time"
fn* count(n) {
  let i = 1
  while (i <= n) {
    yield i
    i = i + 1
  }
}
for await (x in count(3)) {
  print(x)
}
fn* echo() {
  let a = yield "ready"
  print(a)
  let b = yield "more"
  print(b)
}
let g = echo()
print(g.next())
print(g.next("one"))
g.next("two")
async fn* ticks() {
  yield 1
  await time.sleep(1)
  yield 2
  yield 3
}
async fn* doubled(src) {
  for await (n in src) {
    yield n * 2
  }
}
async fn main() {
  for await (v in doubled(ticks())) {
    print(v)
  }
}
await main()
