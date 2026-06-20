// docs_site_gen.as — a mini static-site generator.
//
// Renders a baked set of markdown pages to sanitized HTML, then packages them
// into a DETERMINISTIC tar.gz (mtime/uid/gid zeroed, sorted adds) so the same
// inputs always produce a byte-identical artifact. It prints only stable data —
// entry names + sizes, and a content-hash equality check across two builds —
// never absolute paths or timestamps.
//
// Uses std/markdown (render) + std/archive (in-memory tarWriter) + std/fs (a
// unique temp output dir, written but not printed) + std/encoding (hash compare).
import * as markdown from "std/markdown"
import * as archive from "std/archive"
import * as fs from "std/fs"
import * as os from "std/os"
import * as uuid from "std/uuid"
import * as array from "std/array"
import * as object from "std/object"
import * as string from "std/string"
import { hexEncode, utf8Decode } from "std/encoding"
import { sha256 } from "std/crypto"

// The baked source pages (filename → markdown body). Insertion order is fixed,
// but we sort before adding so the archive is order-independent too.
let pages = {"index.md": "# Home\n\nWelcome to the **demo** site.\n\n- [Guide](guide.html)\n- [About](about.html)\n", "guide.md": "## Guide\n\nRun `ascript run app.as`. Tables work:\n\n| Step | Action |\n|---|---|\n| 1 | build |\n| 2 | ship |\n", "about.md": "## About\n\nUser-submitted note: <script>alert(1)</script> — rendered INERT by the sanitizer.\n"}

let pageTemplate = (title, body) => `<!doctype html><html><head><title>${title}</title></head><body>\n${body}</body></html>\n`

// Build the site into a deterministic tar.gz and return its bytes.
fn buildSite() {
  let w = archive.tarWriter({gzip: true, deterministic: true})
  // Sort the page names so add-order never affects the output.
  let names = array.sort(object.keys(pages))
  for (name of names) {
    let slug = string.replace(name, ".md", "") // "index.md" -> "index"
    let html = pageTemplate(slug, markdown.render(pages[name]))
    // `add` succeeds with nil (a bad name/data is a Tier-2 panic — programmer
    // error — not a recoverable result), so no destructuring here.
    w.add(`${slug}.html`, html)
  }
  // `finish` returns the archive bytes directly (Tier-2 on a finalize failure).
  return w.finish()
}

async fn main() {
  // Build twice — deterministic mode must produce identical bytes.
  let tgz1 = buildSite()
  let tgz2 = buildSite()
  print(`reproducible: ${hexEncode(sha256(tgz1)) == hexEncode(sha256(tgz2))}`)

  // Read the generated entries back (gzip auto-sniffed) and print stable info.
  let entries = []
  for await (e in archive.tarEntries(tgz1)) {
    entries = [...entries, `${e.name} (${e.size}B)`]
  }
  print("generated pages:")
  for (e of array.sort(entries)) {
    print(`  ${e}`)
  }

  // Confirm the sanitizer ran: about.html must NOT contain a live <script>.
  let aboutOk = false
  for await (e in archive.tarEntries(tgz1)) {
    if (e.name == "about.html") {
      let body = utf8Decode(e.data)!
      aboutOk = !string.contains(body, "<script>")
    }
  }
  print(`about.html sanitized: ${aboutOk}`)

  // Write the artifact to a unique temp path (never printed) and clean up — the
  // showcase is "this is a real deployable bundle", but the path stays out of
  // the deterministic output.
  let outDir = fs.join(os.tempDir(), `ascript-site-${uuid.v4()}`)
  let [_m, mkErr] = fs.mkdir(outDir, {recursive: true})
  if (mkErr == nil) {
    let [_w, wErr] = fs.write(fs.join(outDir, "site.tar.gz"), tgz1)
    print(`artifact written: ${wErr == nil}`)
    fs.remove(outDir, {recursive: true})
  }
}

await main()
print("docs_site_gen ok")
