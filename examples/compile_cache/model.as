// A tiny data layer: the leaf module of the compile-cache demo graph
// (main.as -> util.as -> model.as). See main.as for the cache notes.

export fn greet(name: string): string {
  return "hello " + name
}

export fn shout(name: string): string {
  return greet(name) + "!"
}
