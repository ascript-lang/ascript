import * as string from "std/string"
import * as array from "std/array"
import * as object from "std/object"
import * as map from "std/map"
import * as math from "std/math"
import * as convert from "std/convert"

// string + array + core
let words = string.split("the quick brown fox", " ")
print(len(words))
let lengths = array.map(words, (w) => len(w))
print(lengths)
print(array.reduce(lengths, (a, n) => a + n, 0))
print(string.join(array.sort(words), ", "))

// math + range
let squares = array.map(range(1, 5), (n) => math.pow(n, 2))
print(squares)
print(math.max(3, 9, 2))

// object
let person = { name: "Ada", age: 36 }
print(object.keys(person))
print(object.has(person, "age"))

// map<K,V> type + std/map
let scores: map<string, number> = map.new()
map.set(scores, "ada", 100)
map.set(scores, "alan", 95)
print(map.get(scores, "ada"))
print(len(scores))

// convert + destructuring of a Tier-1 Result
let [n, err] = convert.parseNumber("42")
if (err == nil) {
  print(n + 8)
}
let [bad, e2] = convert.parseNumber("xyz")
print(e2.message)
