let n = 5
let result = 1
for (i in 1..6) {
  result *= i
}
if (result > 100) {
  print("big")
} else {
  print("small")
}
print(result)
