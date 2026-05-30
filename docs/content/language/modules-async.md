:::eyebrow Language

# Modules & async

## Modules

One file is one module. Use `export` to expose bindings and `import` to pull them in. There are no
default exports.

```ascript
// util.as
export const PI = 3.14159
export fn double(x) { return x * 2 }
fn secret() { return 99 }            // not exported — private to this module
```

```ascript
// main.as
import { PI, double } from "./util"     // named import
import * as util from "./util"          // namespace import

print(double(21))      // 42
print(util.PI)         // 3.14159
```

- **Relative paths** (`"./util"`, `"../lib/helpers"`) resolve against the importing file's directory.
  The `.as` extension is implied.
- **Standard-library paths** (`"std/json"`, `"std/net/http"`) resolve to built-in modules.
- Importing a name a module does not export is an error.

```ascript
import { get, post } from "std/net/http"
import * as json from "std/json"
```

Each module is evaluated **once** and cached. A circular import resolves to the partially-initialized
module — using a binding before it has been initialized is a load-order error.

### The always-global core

A handful of builtins need no import and are available everywhere: `print`, `len`, `type`, `assert`,
`range`, `Ok`, `Err`, `recover`. Everything else lives in a `std/*` module.

## Async

AScript supports `async fn` and `await` on a **single-threaded event loop** — a single-threaded Tokio
runtime that *is* the loop. There is no second thread and no user-visible task-spawning primitive, so
there are no data races to reason about.

```ascript
async fn fetchUser(id: number): Result<object> {
  let [resp, err] = await get(`https://api.example.com/users/${id}`)
  if (err != nil) { return Err(err.message) }
  return await resp.json()
}

let [user, err] = await fetchUser(42)
```

- `await expr` suspends until `expr` resolves. You can `await` any value — `await 5` is just `5`.
- Async standard-library functions (timers, sockets, HTTP, WebSockets, subprocess I/O) return
  awaitables driven by the runtime.
- Purely synchronous programs never touch the executor and pay no async cost.
- Async composes with [results](errors): `await someCall()?` awaits, then propagates on error.

### Top-level await

The top level of a program may use `await` directly, or you can define and await a `main`:

```ascript
import { listen } from "std/net/tcp"

async fn main() {
  let [server, err] = await listen("127.0.0.1", 8080)
  // …
}

await main()
```

> [!NOTE] Because execution is sequential and single-threaded, two awaited operations in a row run
> one after the other — there is no built-in `Promise.all` / task-spawn. For concurrent clients and
> servers (an HTTP round-trip, a WebSocket handshake), run the server and client as **separate
> processes**. See [Networking & HTTP](../stdlib/net) and the [Examples](../examples).
