import * as str from "std/string"
async fn* tokens() {
  let words = ["Hello", " world", ". ", "Streaming", " is", " fun", "."]
  for (w of words) {
    yield w
  }
}
fn last_char(s) {
  if (len(s) == 0) {
    return ""
  }
  return str.slice(s, len(s) - 1, len(s))
}
async fn* sentences(src) {
  let buf = ""
  for await (tok in src) {
    buf = buf + tok
    let last = last_char(str.trim(buf))
    if (last == "." || last == "!") {
      yield str.trim(buf)
      buf = ""
    }
  }
  if (str.trim(buf) != "") {
    yield str.trim(buf)
  }
}
async fn* enumerate(src) {
  let i = 0
  for await (item in src) {
    yield { index: i, value: item }
    i = i + 1
  }
}
fn render(event) {
  return `event ${event.index}: ${event.value}`
}
let pipeline = enumerate(sentences(tokens()))
for await (event in pipeline) {
  print(render(event))
}
