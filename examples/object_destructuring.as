// Object destructuring: shorthand, rename with `as`, quoted keys, missing → nil.
let user = {name: "Ada", role: "admin", "login count": 42}

let {name, role as r} = user
print(name)           // Ada
print(r)              // admin

let {"login count" as logins, missing} = user
print(logins)         // 42
print(missing)        // nil

// Works on class instances too.
class Point {
  x: number
  y: number
}
let p = Point.from({x: 3, y: 4})
let {x, y} = p
print(x + y)          // 7
