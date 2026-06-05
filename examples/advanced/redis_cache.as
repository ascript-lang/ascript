// redis_cache.as — a fully error-handled std/redis tour.
//
// Connect, set/get/incr/del with a unique key prefix, and close. No bundled Redis,
// so this example is a no-op (prints a skip note, exits 0) unless ASCRIPT_TEST_REDIS_URL
// is set, e.g.:
//
//   docker run -p 6379:6379 -d redis
//   ASCRIPT_TEST_REDIS_URL=redis://localhost ascript run examples/advanced/redis_cache.as
import * as redis from "std/redis"
import * as env from "std/env"

async fn main() {
  let url = env.get("ASCRIPT_TEST_REDIS_URL")
  if (url == nil) {
    print("redis_cache: ASCRIPT_TEST_REDIS_URL not set — skipping live demo (ok)")
    return
  }

  let [conn, cerr] = await redis.connect(url)
  if (cerr != nil) {
    print(`connect failed: ${cerr.message}`)
    return
  }

  // Unique key prefix so concurrent runs against a shared server don't collide.
  let key = "sp5:cache:demo"

  let [_s, e1] = await conn.set(key, "hello")
  if (e1 != nil) { print(`set failed: ${e1.message}`); conn.close(); return }

  let [v, e2] = await conn.get(key)
  if (e2 != nil) { print(`get failed: ${e2.message}`); conn.close(); return }
  print(`get ${key} = ${v}`)

  // A generic command works too (here: counter via INCR).
  let counter = "sp5:cache:counter"
  await conn.del(counter)
  let [n1, _e3] = await conn.incr(counter)
  let [n2, _e4] = await conn.command("INCR", counter)
  print(`counter after two incrs = ${n2}`)

  let [exists, _e5] = await conn.exists(key)
  print(`exists ${key} = ${exists}`)

  // Clean up the demo keys.
  await conn.del(key)
  await conn.del(counter)
  conn.close()
  print("redis_cache: done")
}

await main()
