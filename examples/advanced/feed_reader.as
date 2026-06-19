// feed_reader.as — parse an RSS feed with std/xml, then sanitize every item's
// HTML description with std/html before display.
//
// The real-world shape: a feed is XML (std/xml, strict), but item descriptions
// embed untrusted HTML — you must NEVER render that raw. std/html.sanitize is an
// allowlist-based, fail-closed sanitizer: only safe formatting tags survive,
// scheme-checked links are kept, and everything else (script, event handlers,
// javascript: URLs) is stripped or escaped. This is the canonical xml→html
// pipeline.
import * as xml from "std/xml"
import * as html from "std/html"

// A baked RSS 2.0 feed. Two items; the second has a hostile description.
const FEED = "<rss version=\"2.0\"><channel>" + "<title>AScript Weekly</title>" + "<item>" + "<title>Bytecode VM ships</title>" + "<description>&lt;p&gt;The &lt;b&gt;async VM&lt;/b&gt; is now the default engine.&lt;/p&gt;</description>" + "</item>" + "<item>" + "<title>Security note</title>" + "<description>&lt;p&gt;Click &lt;a href=\"javascript:steal()\"&gt;here&lt;/a&gt; " + "&lt;script&gt;alert(1)&lt;/script&gt;or &lt;a href=\"https://example.com\"&gt;the docs&lt;/a&gt;.&lt;/p&gt;</description>" + "</item>" + "</channel></rss>"

// Pull the text of the first child element named `name` under `el`.
fn childText(el, name) {
  for (c of el.children) {
    // skip text nodes (plain strings have no .tag)
    if (type(c) == "object" && c.tag == name) {
      if (len(c.children) > 0) {
        return c.children[0]
      }
      return ""
    }
  }
  return ""
}

fn main() {
  let [doc, err] = xml.parse(FEED)
  if (err != nil) {
    print(`feed parse error: ${err.message}`)
    return
  }
  // <rss> → <channel> → many <item>
  let channel = doc.children[0]
  print(`feed: ${childText(channel, "title")}`)
  let count = 0
  for (item of channel.children) {
    if (type(item) != "object" || item.tag != "item") {
      continue
    }
    count = count + 1
    let title = childText(item, "title")
    // The description is HTML-as-text (the feed escaped it). xml.parse already
    // un-escaped the entities, so `raw` is real HTML markup we must sanitize.
    let raw = childText(item, "description")
    let safe = html.sanitize(raw)
    print(`  [${count}] ${title}`)
    print(`      ${safe}`)
  }
  print(`items: ${count}`)
}

main()
print("feed_reader ok")
