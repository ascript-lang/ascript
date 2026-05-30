// ws_client.as — connects to the ws_server.as echo server, sends a few
// messages, prints each echoed reply, then closes cleanly.
//
//   ascript run examples/advanced/ws_server.as   # terminal 1 (start first)
//   ascript run examples/advanced/ws_client.as   # terminal 2
//
// If the server isn't up, connect() returns a Tier-1 error and we exit cleanly.

import { connect } from "std/net/ws"
import * as encoding from "std/encoding"

const URL = "ws://127.0.0.1:8788"

// Send one text message and await the echoed reply.
async fn roundtrip(conn, text) {
  let [_, serr] = await conn.send(text)
  if (serr != nil) {
    print(`send failed: ${serr.message}`)
    return false
  }
  let [reply, rerr] = await conn.recv()
  if (rerr != nil || reply == nil) {
    print("no reply (connection closed)")
    return false
  }
  print(`sent "${text}"  ->  got "${reply}"`)
  return true
}

async fn main() {
  let [conn, err] = await connect(URL, { headers: { "x-client": "ascript-demo" } })
  if (err != nil) {
    print(`could not connect to ${URL} — ${err.message}`)
    print("(start examples/advanced/ws_server.as first)")
    return
  }
  print(`connected to ${URL}`)

  for (word of ["hello", "websockets", "from", "ascript"]) {
    if (!await roundtrip(conn, word)) { break }
  }

  // Send one binary frame too.
  let bin = encoding.utf8Encode("raw bytes")
  let [_, berr] = await conn.send(bin)
  if (berr == nil) {
    let [echoed, _e] = await conn.recv()
    print(`binary echo: ${len(echoed)} bytes`)
  }

  conn.close()
  print("closed")
}

await main()
