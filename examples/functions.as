fn fib(n) {
  if (n < 2) { return n }
  return fib(n - 1) + fib(n - 2)
}

let nums = 0
for (i in 0..10) {
  if (fib(i) % 2 == 0) { continue }
  nums += 1
}

let triple = x => x * 3
print(fib(10))
print(triple(7))
print(nums)
