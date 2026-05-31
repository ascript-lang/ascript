import * as log from "std/log"

log.setLevel("debug")
log.debug("starting", {pid: 42})
log.info("request", {method: "GET", path: "/users", ms: 12})
log.warn("slow query", {ms: 540})
log.error("upstream failed", {code: 502})

// Switch to JSON-lines for ingestion.
log.setFormat("json")
log.info("saved", {userId: 7, ok: true})

// Thunk: only evaluated if the level passes (debug is on here).
// Emits in JSON format, since setFormat("json") preceded it.
log.debug(() => "computed detail")
