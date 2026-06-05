// crypto_and_compress.as
// ---------------------------------------------------------------------------
// A tour of std/crypto, std/encoding and std/compress:
//   - SHA-256 and HMAC-SHA256 digests
//   - cryptographically random bytes
//   - password hashing (argon2) + verification
//   - base64 / hex encoding of raw bytes
//   - gzip round-trip with an integrity assertion
//   - zip archive create + extract round-trip
//
// Notes on the error model used here:
//   crypto.sha256 / hmacSha256        -> string (hex), never fail
//   crypto.randomBytes(n)             -> bytes
//   crypto.hashPassword(pw)           -> [phcString, err]
//   crypto.verifyPassword(pw, phc)    -> bool
//   encoding.base64Encode/hexEncode   -> string
//   compress.gzip(data)               -> bytes
//   compress.gunzip(bytes)            -> [bytes, err]
//   compress.zipCreate([...])         -> [bytes, err]
//   compress.zipExtract(bytes)        -> [entries, err]
// ---------------------------------------------------------------------------

import * as crypto from "std/crypto"
import * as encoding from "std/encoding"
import * as compress from "std/compress"
import * as string from "std/string"

fn main() {
  print("=== Hashing ===")
  let message = "The quick brown fox jumps over the lazy dog"
  let digest = crypto.sha256(message)
  print(`sha256(message)     = ${digest}`)

  // HMAC needs a key + data; we use a fixed key here for a reproducible tag.
  let tag = crypto.hmacSha256("secret-key", message)
  print(`hmacSha256(key,msg) = ${tag}`)

  print("\n=== Random bytes / encoding ===")
  let rnd = crypto.randomBytes(32)
  print(`randomBytes(32) len = ${len(rnd)}`)
  // These are byte->text encodings, handy for storing/transmitting binary.
  print(`base64              = ${encoding.base64Encode(rnd)}`)
  print(`hex                 = ${encoding.hexEncode(rnd)}`)

  print("\n=== Password hashing ===")
  let password = "correct horse battery staple"
  let [phc, hErr] = crypto.hashPassword(password)
  if (hErr != nil) {
    print(`hashPassword failed: ${hErr.message}`)
    return
  }
  // The PHC string is self-describing (algorithm + params + salt + hash).
  print(`phc string          = ${phc}`)
  let goodMatch = crypto.verifyPassword(password, phc)
  let badMatch = crypto.verifyPassword("wrong password", phc)
  print(`verify (correct)    = ${goodMatch}`)
  print(`verify (wrong)      = ${badMatch}`)
  assert(goodMatch == true)
  assert(badMatch == false)

  print("\n=== gzip round-trip ===")
  // A longish, repetitive string so gzip visibly shrinks it.
  let original = string.repeat("AScript loves robust error handling. ", 40)
  let gz = compress.gzip(original)
  print(`original bytes      = ${len(original)}`)
  print(`gzipped bytes       = ${len(gz)}`)

  let [restoredBytes, gErr] = compress.gunzip(gz)
  if (gErr != nil) {
    print(`gunzip failed: ${gErr.message}`)
    return
  }
  // gunzip yields bytes; decode back to a string to compare.
  let [restored, dErr] = encoding.utf8Decode(restoredBytes)
  if (dErr != nil) {
    print(`utf8 decode failed: ${dErr.message}`)
    return
  }
  assert(restored == original)
  print(`round-trip matches  = ${restored == original}`)

  print("\n=== zstd + brotli codecs ===")
  // zstd and brotli accept a string or bytes and return bytes; the matching
  // *Decompress is Tier-1 ([bytes, err]). An optional 2nd arg tunes the level.
  let zc = compress.zstdCompress(original, 19)
  let [zback, zcErr] = compress.zstdDecompress(zc)
  if (zcErr != nil) { print(`zstd failed: ${zcErr.message}`); return }
  assert(encoding.utf8Decode(zback)[0] == original)
  print(`zstd  ${len(original)} -> ${len(zc)} bytes (round-trip ok)`)

  let bc = compress.brotliCompress(original)
  let [bback, bcErr] = compress.brotliDecompress(bc)
  if (bcErr != nil) { print(`brotli failed: ${bcErr.message}`); return }
  assert(encoding.utf8Decode(bback)[0] == original)
  print(`brotli ${len(original)} -> ${len(bc)} bytes (round-trip ok)`)

  print("\n=== tar archive ===")
  // tarCreate/tarExtract use the SAME {name, data} entry shape as zip.
  let [tarBytes, tcErr] = compress.tarCreate([
    { name: "hello.txt", data: "hello from tar" },
    { name: "raw.bin", data: encoding.hexEncode("ff00") },
  ])
  if (tcErr != nil) { print(`tarCreate failed: ${tcErr.message}`); return }
  print(`tar archive bytes   = ${len(tarBytes)}`)
  let [tarEntries, teErr] = compress.tarExtract(tarBytes)
  if (teErr != nil) { print(`tarExtract failed: ${teErr.message}`); return }
  print(`extracted ${len(tarEntries)} tar entr(ies)`)

  print("\n=== zip archive ===")
  // Build an in-memory zip from two named entries, then read it back.
  let [zipBytes, zErr] = compress.zipCreate([
    { name: "readme.txt", data: "hello from ascript" },
    { name: "data/notes.txt", data: "line one\nline two\n" },
  ])
  if (zErr != nil) {
    print(`zipCreate failed: ${zErr.message}`)
    return
  }
  print(`zip archive bytes   = ${len(zipBytes)}`)

  let [entries, xErr] = compress.zipExtract(zipBytes)
  if (xErr != nil) {
    print(`zipExtract failed: ${xErr.message}`)
    return
  }
  print(`extracted ${len(entries)} entr(ies):`)
  for (e of entries) {
    // each entry is { name, data:<bytes> }
    let [text, tErr] = encoding.utf8Decode(e.data)
    let preview = "<binary>"
    if (tErr == nil) {
      preview = text
    }
    print(`  - ${e.name} (${len(e.data)} bytes): ${preview}`)
  }
}

main()
