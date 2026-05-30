import { sse } from "std/net/http"
import * as env from "std/env"
import * as string from "std/string"
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
    let preview = len(event.data) > 80 ? string.slice(event.data, 0, 80) + "…" : event.data
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
