// The middle module: re-exports a friendly wrapper over model.as.

import { greet, shout } from "./model"

export fn run(name: string): string {
  return greet(name)
}

export fn run_loud(name: string): string {
  return shout(name)
}
