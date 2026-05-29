let people = [
  { name: "Ada", age: 36 },
  { name: "Alan", age: 41 },
  { name: "Grace", age: 45 },
]

let total = 0
let count = 0
for (p of people) {
  total += p.age
  count += 1
}

let oldest = people[0]
for (p of people) {
  if (p.age > oldest.age) { oldest = p }
}

print(`sum of ages: ${total}`)
print(`average: ${total / count}`)
print(oldest.name)
