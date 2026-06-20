// markdown_render.as — CommonMark → HTML, sanitized by DEFAULT.
//
// `markdown.render` runs pulldown-cmark (CommonMark + GFM tables, strikethrough,
// and task-lists on by default) and then pipes the output through the HTML
// sanitizer UNLESS `{sanitize: false}`. So embedded raw HTML, `<script>` tags,
// and `javascript:` links come out INERT — safe to serve user-authored markdown.
// `markdown.escape` neutralizes markdown metacharacters in plain text.
import * as markdown from "std/markdown"

// Basic CommonMark.
let basic = "# Title\n\nSome **bold** and *italic* text with `code`.\n\n- one\n- two\n"
print("--- basic ---")
print(markdown.render(basic))

// GFM extensions: a table, strikethrough, a task list.
let gfm = "| A | B |\n|---|---|\n| 1 | 2 |\n\n~~gone~~\n\n- [x] done\n- [ ] todo\n"
print("--- gfm ---")
print(markdown.render(gfm))

// Sanitize-by-default: a <script> tag and a javascript: link in the SOURCE come
// out inert. The script body is escaped to text; the dangerous href is dropped.
let hostile = "Hello [click me](javascript:alert(1)) world.\n\n<script>steal()</script>\n\nA safe [link](https://example.com).\n"
print("--- sanitized (default) ---")
print(markdown.render(hostile))

// The escape hatch — `{sanitize: false}` — passes raw HTML through UNCHANGED.
// Use ONLY for fully trusted input (the docs carry the XSS warning).
print("--- raw (sanitize:false, trusted input only) ---")
print(markdown.render("<div class=\"note\">trusted html</div>\n", {sanitize: false}))

// markdown.escape — neutralize metacharacters so literal text renders verbatim.
print("--- escape ---")
print(markdown.escape("a *literal* _string_ with [brackets] and # hash"))

print("markdown_render ok")
