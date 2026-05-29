import * as regex from "std/regex"

// compile returns [regex, err]; an invalid pattern yields a Tier-1 error
let [digits, err] = regex.compile("\\d+")
print(type(digits))

// test: does the pattern match anywhere?
print(regex.test(digits, "order #42"))
print(regex.test(digits, "no numbers here"))

// findAll: every whole-match substring
print(regex.findAll(digits, "a1 b22 c333"))

// find: the first match with capture groups and a (char) index
let [pair, e2] = regex.compile("(\\w+)=(\\d+)")
let m = regex.find(pair, "x=10, y=20")
// `match` is a reserved word, so read that key with bracket indexing
print(m["match"])
print(m.index)
print(m.groups)

// replace: all matches, with $N group references
let [kv, e3] = regex.compile("(\\w+)=(\\d+)")
print(regex.replace(kv, "x=10, y=20", "$1:$2"))

// split on a pattern
let [comma, e4] = regex.compile(",\\s*")
print(regex.split(comma, "a, b,c"))

// functions also accept an inline pattern string (compiled on the fly)
print(regex.findAll("[A-Z]", "aBcDe"))
