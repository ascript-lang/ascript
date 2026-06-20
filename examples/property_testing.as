// Property-based testing with std/test: generators + the prop() runner.
//
// Generators are inert tagged Objects you compose with combinators. The prop()
// runner draws concrete values from them with a deterministic, edge-biased
// sampler and checks that a property holds across many random inputs.
//
// IMPORTANT: a property must RETURN A BOOL. A passing assert.* returns nil
// (falsy), which the runner treats as a FAILURE — so write properties that
// return a boolean expression (see the predicates below).
//
// Every prop() here passes an explicit { seed: N } so its run is byte-stable.
// Run the properties with:  ascript test examples/property_testing.as
// A plain `ascript run` only executes the top-level code below (registered
// props/tests run under `ascript test`), which is why the tour prints inline.
import { prop, gen } from "std/test"
import * as array from "std/array"

// ── Generator tour: generators are plain tagged Objects (inert until drawn) ──
let smallInt = gen.int(0, 100)
let coin = gen.bool()
let intList = gen.arrayOf(gen.int())
let user = gen.objectWith({id: gen.int(1, 999), active: gen.bool()})

print(`smallInt: ${smallInt.__gen}`)
print(`coin: ${coin.__gen}`)
print(`intList: ${intList.__gen}`)
print(`user: ${user.__gen}`)
print(`smallInt bounds: ${smallInt.min}..${smallInt.max}`)

// ── Property predicates (each returns a bool) ──

// Structural element-wise equality for int arrays (top-level == on arrays is
// reference equality, so compare element by element).
fn intArrayEq(a, b) {
  if (len(a) != len(b)) {
    return false
  }
  let i = 0
  while (i < len(a)) {
    if (a[i] != b[i]) {
      return false
    }
    i = i + 1
  }
  return true
}

fn additionCommutes(a, b) {
  return a + b == b + a
}

fn reverseTwiceIsIdentity(xs) {
  return intArrayEq(array.reverse(array.reverse(xs)), xs)
}

// Show the predicates hold on concrete inputs (deterministic top-level output).
print(`3 + 4 == 4 + 3: ${additionCommutes(3, 4)}`)
print(`reverse(reverse([1,2,3])) == [1,2,3]: ${reverseTwiceIsIdentity([1, 2, 3])}`)

// ── Registered properties (run under `ascript test`, each with an explicit seed) ──
prop("addition commutes", [gen.int(), gen.int()], additionCommutes, {seed: 1})
prop("reverse twice is identity", [gen.arrayOf(gen.int())], reverseTwiceIsIdentity, {seed: 2})

print("property tour complete")
