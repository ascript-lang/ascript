export const PI = 3.14159

export fn circleArea(r) {
  return PI * r * r
}

export class Rect {
  fn init(w, h) { self.w = w; self.h = h }
  fn area() { return self.w * self.h }
}
