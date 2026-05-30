// ws_server.as — a WebSocket echo server built on std/net/ws.
//
// Accepts connections one at a time and echoes every message back, upper-cased
// for text frames, verbatim for binary frames, until the peer closes.
//
// Run this, then connect with the companion client (separate process — the
// handshake needs the server's accept loop running):
//   ascript run examples/advanced/ws_server.as   # terminal 1
//   ascript run examples/advanced/ws_client.as   # terminal 2

import { listen } from "std/net/ws"
import * as string from "std/string"

const HOST = "127.0.0.1"
const PORT = 8788

// Serve one connected client to completion: echo until recv() yields nil.
async fn serveClient(conn) {
  let [msg, err] = await conn.recv()
  while (msg != nil) {
    if (err != nil) {
      print(`recv error: ${err.message}`)
      break
    }
    if (type(msg) == "string") {
      print(`text  <- ${msg}`)
      await conn.send(string.upper(msg))   // echo upper-cased
    } else {
      print(`binary <- ${len(msg)} bytes`)
      await conn.send(msg)                 // echo verbatim
    }
    let r = await conn.recv()
    msg = r[0]
    err = r[1]
  }
  conn.close()
  print("client disconnected")
}

async fn main() {
  let [server, err] = await listen(HOST, PORT)
  if (err != nil) {
    print(`could not bind ws://${HOST}:${PORT} — ${err.message}`)
    return
  }
  print(`ws echo server on ws://${HOST}:${server.port}  (Ctrl-C to stop)`)

  while (true) {
    let [conn, aerr] = await server.accept()
    if (aerr != nil) {
      print(`accept failed: ${aerr.message}`)
      break
    }
    print("client connected")
    await serveClient(conn)
  }
  server.close()
}

await main()
