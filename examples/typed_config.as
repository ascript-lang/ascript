// Typed parse for TOML / YAML / CSV (SP5 §3).
//
// Like json.parse(text, Class), the toml/yaml/csv parsers accept a 2nd Class (or
// schema) argument: they decode the document and validate it against the type in
// one step, fusing a decode failure and a shape mismatch into ONE [value, err].
import * as toml from "std/toml"
import * as yaml from "std/yaml"
import * as csv from "std/csv"

class Config {
  host: string
  port: number
}

// ── TOML → class ──────────────────────────────────────────────────────────
let [cfg, terr] = toml.parse("host = \"localhost\"\nport = 8080", Config)
assert(terr == nil, `toml typed: err should be nil, got ${terr}`)
assert(cfg.host == "localhost", "toml host")
assert(cfg.port == 8080, "toml port")
print(`toml: ${cfg.host}:${cfg.port}`)

// ── YAML → class ──────────────────────────────────────────────────────────
let [ycfg, yerr] = yaml.parse("host: example.com\nport: 443", Config)
assert(yerr == nil, `yaml typed: err should be nil, got ${yerr}`)
assert(ycfg.host == "example.com", "yaml host")
assert(ycfg.port == 443, "yaml port")
print(`yaml: ${ycfg.host}:${ycfg.port}`)

// ── shape mismatch → [nil, err] (no panic) ────────────────────────────────
let [bad, berr] = toml.parse("host = \"x\"\nport = \"not-a-number\"", Config)
assert(bad == nil, "toml mismatch: nil value")
assert(berr != nil, "toml mismatch: err set")
print(`toml mismatch rejected: ${berr.message}`)

// ── CSV rows → class (header mode) ────────────────────────────────────────
class Row {
  name: string
  age: number
}
let [rows, rerr] = csv.parse("name,age\nAda,36\nGrace,37", Row, { header: true })
assert(rerr == nil, `csv typed: err should be nil, got ${rerr}`)
assert(len(rows) == 2, "csv: two rows")
for (r in rows) { print(`csv row: ${r.name} ${r.age}`) }
assert(rows[0].name == "Ada", "csv row 0 name")
assert(rows[1].age == 37, "csv row 1 age coerced to number")

// ── CSV bad cell → row-pathed err ─────────────────────────────────────────
let [badrows, crerr] = csv.parse("name,age\nAda,notnum", Row, { header: true })
assert(badrows == nil, "csv bad cell: nil value")
assert(crerr != nil, "csv bad cell: err set")
print(`csv bad cell rejected: ${crerr.message}`)

print("typed_config: all assertions passed")
