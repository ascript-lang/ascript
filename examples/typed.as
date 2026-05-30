fn area(width: number, height: number): number {
  return width * height
}

fn greet(name: string): string {
  return `hello, ${name}`
}

let dims: array<number> = [3, 4, 5]
let total: number = 0
for (d of dims) {
  total += d
}

print(area(3, 4))
print(greet("Ada"))
print(total)

// a contract violation, caught by recover
let r = recover(() => area("wide", 4))
print(r[1].message)

// a `future<T>` contract: calling an async fn yields a future; the binding
// is annotated `future<number>`, and awaiting it produces the number.
async fn compute(): number {
  return 42
}
let pending: future<number> = compute()
print(await pending)
