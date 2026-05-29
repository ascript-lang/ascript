// std/net/tcp — self-contained loopback echo demo (no external network).
//
// AScript is single-threaded with an inline async model, so a pure-loopback TCP
// echo would deadlock IF accept() blocked before connect() ran. It does not here:
// connect() completes the TCP handshake into the OS listen backlog WITHOUT a
// matching accept(), so the sequence below runs deterministically end to end:
//
//   listen  -> bind 127.0.0.1:0, read the OS-assigned port
//   connect -> handshake completes into the listen backlog
//   accept  -> dequeues that backlog connection
//   client.write -> server.readLine -> server.write (echo) -> client.readLine
//
// Run:  cargo run --quiet -- run examples/net.as

import * as tcp from "std/net/tcp"

// Bind a listener on an ephemeral port (port 0 -> OS picks a free one).
let [server, e1] = tcp.listen("127.0.0.1", 0)
print(e1)
let port = server.port

// connect() completes into the listen backlog before we accept().
let [client, e2] = await tcp.connect("127.0.0.1", port)
print(e2)

// accept() dequeues the queued connection — no deadlock, single-threaded.
let [conn, e3] = await server.accept()
print(e3)

// Round-trip a line: client -> server.
await client.write("ping\n")
let line = await conn.readLine()
print(line) // ping

// Echo it back: server -> client.
await conn.write("pong\n")
let reply = await client.readLine()
print(reply) // pong

client.close()
conn.close()
server.close()
