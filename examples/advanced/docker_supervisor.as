// docker_supervisor.as — a production-shaped Docker container supervisor (CNTR §10).
//
// It watches the daemon's event stream for `die` events on a supervised label,
// inspects the dead container, restarts it under a bounded `task.retry` budget, and
// streams a tail of the restarted container's logs (demultiplexing stdout + stderr).
// A SIGTERM handler closes the watch and exits cleanly. Structured `std/log` is used
// throughout (it routes to stderr/the log buffer, NOT stdout), while the deterministic
// stdout verdict is emitted with `print` so the run is byte-identical across engines.
//
// Determinism: when run against the recorded-fixture mock daemon, the event stream
// plays a single scripted `die` event then EOF, so the `for await` loop terminates and
// the program finishes on its own (no live daemon, no timestamps/ids in the output).
import * as docker from "std/docker"
import * as env from "std/env"
import * as log from "std/log"
import * as process from "std/process"
import * as task from "std/task"

// The label this supervisor is responsible for restarting.
let SUPERVISED_LABEL = "app=web"

let sock = env.get("DOCKER_SOCK")
if (sock == nil) {
  sock = "/var/run/docker.sock"
}

let [client, connErr] = await docker.connect({socketPath: sock})
if (connErr != nil) {
  print("docker: unavailable")
  exit(0)
}

// A mutable watch handle so the SIGTERM handler can close it on shutdown.
let watch = nil
let shuttingDown = false

// Production-shaped graceful shutdown: SIGTERM closes the event watch + the client.
// (In the deterministic mock run this never fires — the event stream ends on its own.)
process.on("SIGTERM", () => {
  shuttingDown = true
  log.info("supervisor: SIGTERM received, shutting down")
  if (watch != nil) {
    watch.close()
  }
  client.close()
})

// Restart a container under a bounded retry budget; returns `[ok, attempts]`.
async fn restartWithRetry(id) {
  let attempts = 0
  let restarter = async () => {
    attempts = attempts + 1
    let [_, err] = await client.restart(id)
    if (err != nil) {
      assert(false, `restart failed: ${err.message}`)
    }
    return true
  }
  let [_ok, err] = recover(() => {
    await task.retry(restarter, {attempts: 3, baseMs: 1, backoff: "exponential"})
    return nil
  })
  if (err != nil) {
    log.error("supervisor: restart exhausted", {container: id, attempts: attempts})
    return [false, attempts]
  }
  return [true, attempts]
}

// Stream a tail window of the restarted container's logs, demuxing stdout + stderr.
// Returns the number of log lines observed.
async fn tailLogs(id) {
  let [logs, err] = await client.logs(id, {stdout: true, stderr: true, tail: "10"})
  if (err != nil) {
    log.warn("supervisor: could not open logs", {container: id})
    return 0
  }
  let seen = 0
  for await (entry in logs) {
    seen = seen + 1
    log.debug("supervisor: log line", {stream: entry.stream})
  }
  return seen
}

// Watch `die` events for the supervised label and react to each.
let [events, watchErr] = await client.events({filters: {event: ["die"], label: [SUPERVISED_LABEL]}})
if (watchErr != nil) {
  print(`supervisor: watch error ${watchErr.statusCode}`)
  client.close()
  exit(0)
}
watch = events

let handled = 0
let restarted = 0
let totalAttempts = 0
let logLinesSeen = 0

log.info("supervisor: watching", {label: SUPERVISED_LABEL})

for await (ev in events) {
  if (ev.Action != "die") {
    continue
  }
  handled = handled + 1
  let id = ev.Actor.ID
  log.info("supervisor: die event", {container: id})

  // Inspect to confirm the container exists before acting on it.
  let [info, inspectErr] = await client.inspect(id)
  if (inspectErr != nil) {
    log.warn("supervisor: inspect failed", {container: id})
    continue
  }
  let [ok, attempts] = await restartWithRetry(id)
  totalAttempts = totalAttempts + attempts
  if (ok) {
    restarted = restarted + 1
    logLinesSeen = logLinesSeen + await tailLogs(id)
  }
}

// The event stream ended (EOF) — reclaim the watch and the client.
events.close()
client.close()

// Deterministic verdict (counts + names only, no ids/timestamps).
print(`label: ${SUPERVISED_LABEL}`)
print(`events handled: ${handled}`)
print(`restarted: ${restarted}`)
print(`restart attempts: ${totalAttempts}`)
print(`log lines seen: ${logLinesSeen}`)
print("supervisor: done")
