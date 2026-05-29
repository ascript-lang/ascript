// Capstone: every M11 serialization module + the Bytes and Regex kinds.
import * as json from "std/json"
import * as toml from "std/toml"
import * as yaml from "std/yaml"
import * as encoding from "std/encoding"
import * as regex from "std/regex"
import * as uuid from "std/uuid"
import * as csv from "std/csv"
import * as bytes from "std/bytes"

// JSON round-trip + destructuring of the Tier-1 Result.
let [config, e1] = json.parse(`{"name": "ascript", "version": 11, "tags": ["lang", "rust"]}`)
print(config.name)
print(config.version)
print(config.tags[0])
let [j, e2] = json.stringify({ ok: true, n: 3 })
print(j)

// TOML + YAML config parsing.
let [tcfg, e3] = toml.parse("title = \"demo\"\nport = 8080")
print(tcfg.title)
print(tcfg.port)
let [ycfg, e4] = yaml.parse("env: prod\nreplicas: 3")
print(ycfg.env)
print(ycfg.replicas)

// encoding: base64 round-trip + utf8 decode of the raw bytes.
print(encoding.base64Encode("hi"))
let [raw, e5] = encoding.base64Decode("aGVsbG8=")
let [text, e6] = encoding.utf8Decode(raw)
print(text)

// regex: compile (Regex kind), findAll, and a single find result's `text`.
let [re, e7] = regex.compile("\\w+")
print(regex.findAll(re, "the quick fox"))
let m = regex.find(re, "abc 123")
print(m.text)

// csv: parse rows, index into the data row.
let [rows, e8] = csv.parse("a,b\n1,2")
print(rows[1][0])

// bytes: alloc a buffer, write a big-endian uint, read it back as an array.
let buf = bytes.alloc(2)
bytes.writeUint(buf, 0, 513, 2, "be")
print(bytes.toArray(buf))

// uuid: value is random, so print only its length.
print(len(uuid.v4()))
