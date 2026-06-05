// std/lru — a bounded least-recently-used cache (core module).
import { new } from "std/lru"

let cache = new(2)         // capacity 2
cache.set("a", 1)
cache.set("b", 2)
cache.get("a")            // touch "a" → it becomes most-recently-used
cache.set("c", 3)         // capacity exceeded → evict the LRU entry ("b")

assert(cache.has("a") == true, "a kept (was promoted)")
assert(cache.has("b") == false, "b evicted (was LRU)")
assert(cache.has("c") == true, "c inserted")
assert(cache.len() == 2, "size stays at capacity")
assert(cache.get("a") == 1, "a still maps to 1")

// keys() reports LRU→MRU order.
cache.get("c")            // c becomes MRU; order is now a, c
let ks = cache.keys()
print(`keys: ${ks[0]}, ${ks[1]}`)

cache.delete("a")
assert(cache.len() == 1, "after delete")
cache.clear()
assert(cache.len() == 0, "after clear")
print("lru_cache: all assertions passed")
