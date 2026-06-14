// Core-language object/class program for SHAPE negative-space ASO golden.
// No std/* imports — must compile under --no-default-features.
// Exercises: object literals, spread, class with declared fields/defaults,
// C.from, >64-key object (slab->dict demotion), instanceof.

// 1. Simple object literal with spread
let base = { x: 1, y: 2, z: "hello" }
let ext = { ...base, w: true }
print(ext.x)
print(ext.z)
print(ext.w)

// 2. Class with declared typed fields and defaults
class Point {
    x: number = 0
    y: number = 0
    label: string = "origin"
    fn dist() {
        return self.x * self.x + self.y * self.y
    }
}

class Point3D extends Point {
    z: number = 0
    fn dist() {
        return self.x * self.x + self.y * self.y + self.z * self.z
    }
}

// C.from — uses declared fields + defaults
let raw = { x: 3, y: 4, label: "P1" }
let p = Point.from(raw)
print(p.x)
print(p.label)
print(p.dist())
print(p instanceof Point)

let raw3 = { x: 1, y: 2, z: 2, label: "Q" }
let q = Point3D.from(raw3)
print(q.dist())
print(q instanceof Point3D)
print(q instanceof Point)

// 3. >64-key object — triggers slab->dict demotion inside SHAPE
let big = {}
for (i in 0..70) {
    big[`k${i}`] = i
}
print(big[`k0`])
print(big[`k64`])
print(big[`k69`])
