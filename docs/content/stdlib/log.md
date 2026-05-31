:::eyebrow Standard library

# Logging

`std/log` is a tiny leveled, structured logger. It writes one record per call to **stderr**, in either a human-readable format or one JSON object per line for ingestion. Import it as a namespace: `import * as log from "std/log"`.

`std/log` is gated behind the `log` Cargo feature, which is **on by default** (it depends on `data` for JSON serialization).

## Levels

There are four severities, ordered `debug < info < warn < error`:

| Function | Level |
| --- | --- |
| `log.debug(...)` | `debug` |
| `log.info(...)` | `info` |
| `log.warn(...)` | `warn` |
| `log.error(...)` | `error` |

A record is emitted only when its level is **at or above** the current minimum level. The default minimum is `info`, so `log.debug(...)` is silent until you lower the threshold.

### Default level and `ASCRIPT_LOG`

The initial level comes from the `ASCRIPT_LOG` environment variable — one of `debug`, `info`, `warn`, `error` (case-insensitive). If unset or unrecognized, the default is `info`.

```bash
ASCRIPT_LOG=debug ascript run app.as
```

### log.setLevel

Sets the minimum level at runtime.

- **level** `string` — one of `"debug"`, `"info"`, `"warn"`, `"error"`.

```ascript
import * as log from "std/log"
log.setLevel("debug")
log.debug("now visible")
```

### log.setFormat

Selects the output format.

- **format** `string` — `"human"` (default) or `"json"`.

## Record shape

Every logging call builds one record from its arguments:

- **Non-object arguments** are stringified and joined with spaces to form the `msg`.
- **Object arguments** are merged together as structured **fields**.
- The **level** is added automatically.

```ascript
import * as log from "std/log"
log.info("request", "GET", {path: "/users", ms: 12})
```

The reserved keys `level` and `msg` are **always authoritative** — a field named `level` or `msg` in one of your objects cannot clobber them.

## Formats

**Human** (the default) prints `[LEVEL] msg` followed by ` key=value` pairs:

```
[INFO] request GET path=/users ms=12
```

With no message, the trailing space is dropped — `log.warn()` prints just `[WARN]`.

**JSON** prints one object per line, with your fields first and the authoritative `level`/`msg` last:

```json
{"path":"/users","ms":12,"level":"info","msg":"request GET"}
```

## Deferred messages (thunks)

If the **first** argument is a function, it is treated as a thunk and is invoked **only when the level passes the filter** — so an expensive message computation for a filtered-out `debug` call costs nothing. The thunk's return value becomes the `msg`. An `async fn` thunk is awaited to completion.

```ascript
import * as log from "std/log"
log.debug(() => "expensive: " + computeReport())  // not called unless debug is on
```

## Total serialization

Field serialization is **total**: it never panics, no matter what you log. Reference cycles become `"[Circular]"`, functions become `"<function>"`, and `NaN`/`Infinity` become `null`. Logging is always safe to leave in production code.

## Sink

Records are written to **stderr** (so they don't interleave with `print`, which goes to stdout). In tests the interpreter captures log output in an internal buffer instead.
