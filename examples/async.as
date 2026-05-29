async fn fetch(x) {
  return x * 2
}

let r = await fetch(21)
print(r)

print(await 5)

let g = async (n) => n + 1
print(await g(9))

let h = async x => x - 1
print(await h(8))
