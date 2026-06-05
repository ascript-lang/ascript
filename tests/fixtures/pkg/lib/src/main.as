// Entry module for the `lib` package. Proves a package-internal relative import
// (`./helper`) resolves WITHIN the package root via the existing file loader.
import { punctuate } from "./helper"

export fn greet(name) {
  return punctuate("hello " + name)
}
