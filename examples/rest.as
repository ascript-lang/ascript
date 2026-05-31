// Rest parameters — collect trailing args into an array.
fn sum(...nums: array<number>) {
  let total = 0
  for (n in nums) {
    total = total + n
  }
  return total
}
print(sum(1, 2, 3, 4))            // 10
print(sum())                      // 0

fn tagged(label, ...rest) {
  print(label)
  print(rest)
}
tagged("nums", 1, 2)              // nums then [1, 2]

// Rest in destructuring.
let [head, ...tail] = [10, 20, 30]
print(head)                       // 10
print(tail)                       // [20, 30]

let {id, ...meta} = {id: 7, role: "admin", active: true}
print(id)                         // 7
print(meta)                       // {role: "admin", active: true}

// Spread + rest forwarding round-trip.
fn wrap(...args) {
  return sum(...args)
}
print(wrap(5, 6, 7))              // 18
