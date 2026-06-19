// XML parsing & serialization — std/xml.
//
// std/xml is a strict XML 1.0 parser (Tier-1 errors on malformed input). The
// document shape is stable and documented: every element is
//   { tag: string, attrs: object<string,string>, children: array<node> }
// where a node is either an element object or a plain string (text). CDATA is
// folded into text; comments and processing-instructions are dropped. The five
// built-in entities and numeric refs are expanded; external entities are never
// fetched (no billion-laughs by construction).
import * as xml from "std/xml"

fn main() {
  let src = "<catalog>" + "<book id=\"bk101\" lang=\"en\">" + "<title>XML for Everyone</title>" + "<author>A. Writer</author>" + "<price currency=\"USD\">19.95</price>" + "</book>" + "<book id=\"bk102\" lang=\"fr\">" + "<title>Le Café &amp; Co</title>" + "<author>B. Auteur</author>" + "<price currency=\"EUR\">24.50</price>" + "</book>" + "</catalog>"

  // ── parse ────────────────────────────────────────────────────────────────────
  let [doc, err] = xml.parse(src)
  if (err != nil) {
    print(`parse failed: ${err.message}`)
    return
  }
  print(`root: <${doc.tag}> with ${len(doc.children)} books`)

  // Read each <book>: attributes + the text of nested elements.
  for (book of doc.children) {
    let title = book.children[0].children[0]
    let author = book.children[1].children[0]
    let priceEl = book.children[2]
    print(`  ${book.attrs.id} (${book.attrs.lang}): "${title}" by ${author} — ${priceEl.children[0]} ${priceEl.attrs.currency}`)
  }

  // ── stringify (round-trips entities and structure) ───────────────────────────
  let [out, serr] = xml.stringify(doc)
  print(`re-stringify ok: ${serr == nil}`)
  // A re-parse of the serialized form yields the same root tag + book count.
  let [doc2, _] = xml.parse(out)
  print(`round-trip root: <${doc2.tag}> books=${len(doc2.children)}`)

  // Pretty-print with a 2-space indent.
  let [pretty, _p] = xml.stringify(doc2, {indent: 2})
  print("pretty:")
  print(pretty)

  // ── escaping ─────────────────────────────────────────────────────────────────
  print(`escape: ${xml.escape("if a < b && c > d then \"go\"")}`)

  // ── malformed input is a clean Tier-1 error, not a panic ─────────────────────
  let [bad, berr] = xml.parse("<open><unclosed></open>")
  print(`malformed rejected: ${bad == nil && berr != nil}`)
}

main()
print("xml_basics ok")
