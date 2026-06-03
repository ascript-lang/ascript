// Mid module: TRANSITIVELY imports the leaf `util` module, and re-uses its
// exports inside its own exported function + class.
import { scale, TAU } from "./util"

export fn circumference(radius: number): number {
  return scale(radius, TAU)
}

export class Circle {
  radius: number
  fn init(radius) {
    self.radius = radius
  }
  fn around(): number {
    return circumference(self.radius)
  }
}
