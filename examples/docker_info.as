// docker_info.as — connect to the Docker Engine and print deterministic daemon
// facts (CNTR §10.1). The socket path comes from $DOCKER_SOCK with a fallback to
// the conventional daemon socket, so this runs unchanged against a real daemon or
// the recorded-fixture mock. Every code path is fully Tier-1 error-handled: when no
// daemon is reachable it prints `docker: unavailable` and exits cleanly, so the
// output is deterministic whether or not Docker is present.
import * as docker from "std/docker"
import * as env from "std/env"

let sock = env.get("DOCKER_SOCK")
if (sock == nil) {
  sock = "/var/run/docker.sock"
}

let [client, connErr] = await docker.connect({socketPath: sock})
if (connErr != nil) {
  // No reachable daemon (or a version below the floor): deterministic, clean exit.
  print("docker: unavailable")
  exit(0)
}

// Negotiated API version is a stable client field (no daemon round-trip).
print(`apiVersion: ${client.apiVersion}`)

// Daemon version — print only stable, fixture-deterministic fields.
let [ver, verErr] = await client.version()
if (verErr != nil) {
  print(`version: error ${verErr.statusCode}`)
} else {
  print(`engine: ${ver.Version}`)
  print(`os: ${ver.Os}/${ver.Arch}`)
}

// Daemon info — print the stable counters (no timestamps/ids).
let [info, infoErr] = await client.info()
if (infoErr != nil) {
  print(`info: error ${infoErr.statusCode}`)
} else {
  print(`containers: ${info.Containers}`)
  print(`running: ${info.ContainersRunning}`)
  print(`images: ${info.Images}`)
}

// Container listing — print count and names (stable in the fixture), not ids/times.
let [containers, listErr] = await client.containers({all: true})
if (listErr != nil) {
  print(`containers: error ${listErr.statusCode}`)
} else {
  print(`listed: ${len(containers)}`)
  for (c in containers) {
    print(`  ${c.Names[0]} (${c.Image}) ${c.State}`)
  }
}

client.close()
print("done")
