// Spread: arrays, objects, and call arguments.
let base = [1, 2, 3]
let more = [0, ...base, 4]
print(more)                       // [0, 1, 2, 3, 4]

let defaults = {host: "local", port: 80}
let config = {...defaults, port: 443}
print(config)                     // {host: "local", port: 443}

fn sum3(a, b, c) {
  return a + b + c
}
let nums = [10, 20, 30]
print(sum3(...nums))              // 60
