// sse_client.as — a Server-Sent Events consumer built on std/net/http's sse().
//
// sse(url) issues a GET with `Accept: text/event-stream` and returns a stream
// whose .next() yields parsed { event, data, id, retry } objects until the
// stream ends. Auto-reconnect is on by default; we cap it here.
//
// Point it at any SSE endpoint:
//   ascript run examples/advanced/sse_client.as <url>
//
// With no argument it targets a public demo stream. If the endpoint is
// unreachable or serves no events, the program reports that and exits cleanly.

import { sse } from "std/net/http"
import * as env from "std/env"
import * as string from "std/string"

// Allow overriding the target via the SSE_URL environment variable.
const URL = env.get("SSE_URL") ?? "https://stream.wikimedia.org/v2/stream/recentchange"
const MAX_EVENTS = 5

async fn main() {
  print(`connecting to SSE stream: ${URL}`)
  let [stream, err] = await sse(URL, {
    reconnect: false,          // don't auto-reconnect for this short demo
    headers: { "user-agent": "ascript-sse-demo" },
  })
  if (err != nil) {
    print(`could not open stream — ${err.message}`)
    return
  }

  let seen = 0
  let [event, eerr] = await stream.next()
  while (event != nil && seen < MAX_EVENTS) {
    if (eerr != nil) {
      print(`stream error: ${eerr.message}`)
      break
    }
    seen += 1
    let preview = event.data
    if (len(event.data) > 80) { preview = string.slice(event.data, 0, 80) + "…" }
    let idStr = event.id ?? "-"
    print(`#${seen} [${event.event}] id=${idStr}  ${preview}`)

    let r = await stream.next()
    event = r[0]
    eerr = r[1]
  }

  stream.close()
  let lastId = stream.lastEventId ?? "-"
  print(`done — received ${seen} event(s); last id = ${lastId}`)
}

await main()
