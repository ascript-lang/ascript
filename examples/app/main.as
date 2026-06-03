// Entry point: a NAMED import from a local module that itself imports another
// local module (transitive resolution), plus a NAMESPACE import from the leaf.
import { circumference, Circle } from "./shapes"
import * as util from "./util"

// Named function import (resolved through shapes -> util).
print("circumference:", circumference(2))

// Imported class used across files (its method reaches util transitively).
let c = Circle(3)
print("circle around:", c.around())

// Namespace import: call functions and read a const off the module object.
print(util.label("tau", util.TAU))
print("scaled:", util.scale(10, 4))
print("turns:", util.turns(2))

print("app ok")
