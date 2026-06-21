/* =========================================================================
   AScript playground — client driver (WASM spec §5.5)
   - spawns a browser Web Worker that runs the wasm engine off the UI thread
   - Run posts {id, source}; renders the RunResult
   - Stop = worker.terminate() + lazy respawn (kills an infinite loop)
   - examples <select> from an inline, wasm-subset-safe manifest
   - Share writes location.hash = '#code=' + base64url(src); read on load
   No framework, no build step — same conventions as app.js.
   ========================================================================= */

// ---- wasm-subset-safe examples ------------------------------------------
// Every entry uses ONLY the shipped wasm feature set (CORE + data/binary/log/shared):
// no fs/net/process, no workers, no intervals — those refuse on wasm (see
// tooling/playground.md). Sources are inlined here at authoring time.
const EXAMPLES = [
  { title: 'Hello', source: `print("hello, world")\nprint(1 + 2 * 3)\n` },

  { title: 'Factorial loop', source:
`let n = 5
let result = 1
for (i in 1..=n) {
  result *= i
}
print(\`\${n}! = \${result}\`)
` },

  { title: 'Recursion (fib)', source:
`fn fib(n) {
  if (n < 2) { return n }
  return fib(n - 1) + fib(n - 2)
}
for (i in 0..10) {
  print(fib(i))
}
` },

  { title: 'Functional pipeline', source:
`import * as array from "std/array"

let nums = [1, 2, 3, 4, 5, 6, 7, 8]
let evens = array.filter(nums, x => x % 2 == 0)
let doubled = array.map(evens, x => x * 2)
let total = array.reduce(doubled, (a, b) => a + b, 0)
print(\`evens:   \${evens}\`)
print(\`doubled: \${doubled}\`)
print(\`sum:     \${total}\`)
` },

  { title: 'Classes & contracts', source:
`class User {
  id: number
  name: string
  role: string = "guest"
}

let u = User.from({ id: 1, name: "Ada" })
print(\`\${u.name} (\${u.role})\`)

// a wrong type is a runtime contract violation
let bad = recover(() => User.from({ id: "nope", name: "x" }))
print(bad[1].message)
` },

  { title: 'Algebraic enums + match', source:
`enum Shape {
  Circle(radius: float),
  Rect(w: float, h: float),
}

fn area(s: Shape): float {
  return match s {
    Shape.Circle(r) => 3.14159 * r * r,
    Shape.Rect(w, h) => w * h,
  }
}

print(area(Shape.Circle(2.0)))
print(area(Shape.Rect(w: 3.0, h: 4.0)))
` },

  { title: 'JSON round-trip', source:
`import * as json from "std/json"

let doc = { name: "ascript", tags: ["fast", "typed"], stars: 3 }
let text = json.stringify(doc)!
print(text)

let back = json.parse(text)!
print(back.tags[0])
` },

  { title: 'Async & gather', source:
`import { gather } from "std/task"

async fn square(x) {
  return x * x
}

let results = await gather([square(2), square(3), square(4)])
print(results)
` },

  { title: 'Generators', source:
`fn* counter(limit) {
  let i = 0
  while (i < limit) {
    yield i * i
    i += 1
  }
}

for await (v in counter(5)) {
  print(v)
}
` },

  { title: 'Pattern destructuring', source:
`let point = { x: 3, y: 7, label: "p1" }
let { x, y, label } = point
print(\`\${label}: (\${x}, \${y})\`)

let [first, ...rest] = [10, 20, 30, 40]
print(first)
print(rest)
` },

  { title: 'Errors as values', source:
`import * as convert from "std/convert"

fn parseAge(s) {
  let [n, err] = convert.parseInt(s)
  if (err != nil) {
    return [nil, { message: \`not a number: \${s}\` }]
  }
  return [n, nil]
}

let [age, err] = parseAge("42")
print(age)
let [bad, err2] = parseAge("xyz")
print(err2.message)
` },

  { title: 'Frozen shared value', source:
`import { freeze } from "std/shared"

let config = freeze({ host: "localhost", port: 8080, retries: 3 })
print(config.host)
print(config.port)

// a frozen value cannot be mutated
let r = recover(() => { config.port = 9090 })
print(r[1].message)
` },
];

// ---- base64url for the share link --------------------------------------
function b64urlEncode(str) {
  const bytes = new TextEncoder().encode(str);
  let bin = '';
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}
function b64urlDecode(s) {
  s = s.replace(/-/g, '+').replace(/_/g, '/');
  while (s.length % 4) s += '=';
  const bin = atob(s);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return new TextDecoder().decode(bytes);
}

// ---- worker lifecycle (Stop = terminate + lazy respawn) -----------------
let worker = null;
let runSeq = 0;        // monotonically increasing run id
let pending = null;    // the id of the in-flight run, or null

function spawnWorker() {
  worker = new Worker('assets/playground-worker.js', { type: 'module' });
  worker.onmessage = (e) => {
    const { id, result } = e.data;
    if (id !== pending) return; // a stale message from a terminated run — ignore
    pending = null;
    renderResult(result);
    setRunning(false);
  };
  worker.onerror = (e) => {
    if (pending === null) return;
    pending = null;
    renderResult({ ok: false, output: '', error: 'worker error: ' + (e.message || String(e)),
      diagnostics: [], exitCode: null, durationMs: 0 });
    setRunning(false);
  };
}

function ensureWorker() {
  if (!worker) spawnWorker();
  return worker;
}

// ---- DOM helpers --------------------------------------------------------
const $ = (id) => document.getElementById(id);

function setRunning(on) {
  $('run').disabled = on;
  $('stop').disabled = !on;
  if (on) { $('status').textContent = 'running…'; $('status').className = 'pg-status running'; }
}

function escapeHtml(s) {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

function renderResult(result) {
  const out = $('out');
  const status = $('status');
  let html = '';

  if (result.output) html += `<span class="pg-text">${escapeHtml(result.output)}</span>`;

  if (result.error) {
    html += `<span class="pg-err">${escapeHtml(result.error)}</span>`;
  } else if (result.diagnostics && result.diagnostics.length) {
    html += `<span class="pg-err">${escapeHtml(result.diagnostics.join('\n'))}</span>`;
  }

  if (!html) html = '<span class="pg-muted">(no output)</span>';
  out.innerHTML = html;

  const dur = (typeof result.durationMs === 'number') ? result.durationMs.toFixed(1) + ' ms' : '';
  if (result.ok) {
    const code = (result.exitCode === null || result.exitCode === undefined) ? '0' : String(result.exitCode);
    status.textContent = `ok · exit ${code} · ${dur}`;
    status.className = 'pg-status ok';
  } else {
    status.textContent = `error · ${dur}`;
    status.className = 'pg-status err';
  }
}

function run() {
  if (pending !== null) return; // already running — don't interleave
  const source = $('src').value;
  $('out').innerHTML = '';
  setRunning(true);
  const id = ++runSeq;
  pending = id;
  ensureWorker().postMessage({ id, source });
}

function stop() {
  if (worker) { worker.terminate(); worker = null; } // lazy respawn on next Run
  pending = null;
  setRunning(false);
  $('status').textContent = 'stopped';
  $('status').className = 'pg-status err';
}

// ---- share link ---------------------------------------------------------
function share() {
  const src = $('src').value;
  location.hash = '#code=' + b64urlEncode(src);
  const btn = $('share');
  const prev = btn.textContent;
  navigator.clipboard?.writeText(location.href).then(() => {
    btn.textContent = 'Link copied';
    setTimeout(() => { btn.textContent = prev; }, 1400);
  }).catch(() => {
    btn.textContent = 'Link in URL';
    setTimeout(() => { btn.textContent = prev; }, 1400);
  });
}

function loadFromHash() {
  const m = /[#&]code=([^&]+)/.exec(location.hash);
  if (!m) return false;
  try {
    $('src').value = b64urlDecode(m[1]);
    return true;
  } catch (e) {
    return false;
  }
}

// ---- boot ---------------------------------------------------------------
function boot() {
  // examples dropdown
  const sel = $('examples');
  EXAMPLES.forEach((ex, i) => {
    const opt = document.createElement('option');
    opt.value = String(i);
    opt.textContent = ex.title;
    sel.appendChild(opt);
  });
  sel.addEventListener('change', () => {
    const ex = EXAMPLES[Number(sel.value)];
    if (ex) $('src').value = ex.source;
  });

  // initial editor content: a #code= hash wins, else the first example
  if (!loadFromHash()) {
    $('src').value = EXAMPLES[0].source;
  }

  $('run').addEventListener('click', run);
  $('stop').addEventListener('click', stop);
  $('share').addEventListener('click', share);

  // cmd/ctrl-Enter runs
  $('src').addEventListener('keydown', (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') { e.preventDefault(); run(); }
  });

  // warm the worker so the first Run is fast
  ensureWorker();
}

document.addEventListener('DOMContentLoaded', boot);
