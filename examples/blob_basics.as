// blob_basics.as — std/blob (S3-compatible) put / get / list, verified end-to-end
// by an in-script S3 stub that RECOMPUTES the AWS Signature Version 4 server-side.
//
// The showcase is the stub itself: it does NOT trust the client. For every
// request it rebuilds the SigV4 canonical request from the wire (method, path,
// query, the signed headers), derives the signing key with the four-HMAC chain
// (crypto.hmacSha256 + hex round-tripping), computes the expected signature, and
// rejects (403) anything whose Authorization header does not match. A passing
// put/get/list is therefore proof that std/blob signs correctly.
//
// Single-threaded model: the blob client runs as a spawned task while the main
// task drives the bounded serve loop, so the program runs to completion (no
// blocking accept loop — the server stops after the expected request count).
import { create } from "std/http/server"
import * as blob from "std/blob"
import * as string from "std/string"
import * as crypto from "std/crypto"
import { utf8Decode, hexDecode } from "std/encoding"
import * as task from "std/task"

// ── S3 stub state ──────────────────────────────────────────────────────────────
const REGION = "us-east-1"
const ACCESS_KEY = "AKIAIOSFODNN7EXAMPLE"
const SECRET_KEY = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
const SERVICE = "s3"
let store = {} // key -> body string

// ── SigV4 recomputation (the server side of the contract) ──────────────────────

// hmac(keyBytes, msg) -> raw bytes, by hex round-tripping crypto.hmacSha256.
fn hmacBytes(keyBytes, msg) {
  return hexDecode(crypto.hmacSha256(keyBytes, msg))!
}

// The four-HMAC SigV4 signing-key derivation: kDate -> kRegion -> kService -> kSigning.
fn signingKey(secret, date, region, service) {
  let kDate = hmacBytes(`AWS4${secret}`, date)
  let kRegion = hmacBytes(kDate, region)
  let kService = hmacBytes(kRegion, service)
  return hmacBytes(kService, "aws4_request")
}

// Canonicalize a header value: trim + collapse internal whitespace to single spaces.
fn canonHeaderValue(v) {
  let parts = string.split(string.trim(v), " ")
  let out = []
  for (p of parts) {
    if (p != "") {
      out = [...out, p]
    }
  }
  return string.join(out, " ")
}

// Rebuild the canonical request and recompute the signature; compare to what the
// client sent in `Authorization`. Returns true iff the signature is valid.
fn verifySig(method, path, query, headers, authHeader) {
  // Authorization = "AWS4-HMAC-SHA256 Credential=AK/DATE/REGION/s3/aws4_request,
  //                  SignedHeaders=h1;h2;..., Signature=hex"
  // Pull out the three fields.
  let credPart = sliceBetween(authHeader, "Credential=", ",")
  let signedHeaders = sliceBetween(authHeader, "SignedHeaders=", ",")
  let theirSig = sliceAfter(authHeader, "Signature=")
  if (credPart == nil || signedHeaders == nil || theirSig == nil) {
    return false
  }

  // Credential = AK/DATE/REGION/SERVICE/aws4_request
  let credBits = string.split(credPart, "/")
  if (len(credBits) != 5) {
    return false
  }
  let date = credBits[1]
  let amzDate = headers["x-amz-date"]
  let payloadHash = headers["x-amz-content-sha256"]
  if (amzDate == nil || payloadHash == nil) {
    return false
  }

  // Canonical headers block (the signed set, in the order SignedHeaders lists them).
  let names = string.split(signedHeaders, ";")
  let canonHeaders = ""
  for (name of names) {
    let val = headers[name]
    if (val == nil) {
      return false
    }
    canonHeaders = canonHeaders + `${name}:${canonHeaderValue(val)}\n`
  }

  // Canonical request.
  let canonReq = `${method}\n${path}\n${query}\n${canonHeaders}\n${signedHeaders}\n${payloadHash}`

  // String-to-sign.
  let scope = `${date}/${REGION}/${SERVICE}/aws4_request`
  let canonReqHash = crypto.sha256(canonReq)
  let stringToSign = `AWS4-HMAC-SHA256\n${amzDate}\n${scope}\n${canonReqHash}`

  // Derive the key + sign.
  let key = signingKey(SECRET_KEY, date, REGION, SERVICE)
  let expected = crypto.hmacSha256(key, stringToSign)
  return crypto.timingSafeEqual(expected, theirSig)
}

// Slice helpers: text between `start..end`, or after `start` to the end.
fn sliceBetween(s, start, end) {
  let i = string.find(s, start)
  if (i < 0) {
    return nil
  }
  let from = i + len(start)
  let rest = string.slice(s, from, len(s))
  let j = string.find(rest, end)
  if (j < 0) {
    return nil
  }
  return string.slice(rest, 0, j)
}
fn sliceAfter(s, start) {
  let i = string.find(s, start)
  if (i < 0) {
    return nil
  }
  return string.slice(s, i + len(start), len(s))
}

// Build the canonical query the same way SigV4 does: sort encoded pairs. The stub
// only handles the simple params S3 list uses (no values needing escaping), so a
// lexical sort of `k=v` pairs matches the client's canonical query.
fn canonicalQuery(queryObj) {
  let pairs = []
  for (k of objectKeys(queryObj)) {
    pairs = [...pairs, `${k}=${queryObj[k]}`]
  }
  return string.join(array.sort(pairs), "&")
}

// Minimal helpers (kept local so the example is self-contained).
import * as object from "std/object"
import * as array from "std/array"
fn objectKeys(o) {
  return object.keys(o)
}

// ── the stub handler ───────────────────────────────────────────────────────────
fn handle(req, method, key) {
  let auth = req.headers["authorization"] ?? ""
  let cq = canonicalQuery(req.query)
  if (!verifySig(method, req.path, cq, req.headers, auth)) {
    return {status: 403, body: "<Error><Code>SignatureDoesNotMatch</Code></Error>"}
  }
  // Signature OK — also confirm the declared payload hash matches the body.
  let declared = req.headers["x-amz-content-sha256"] ?? ""
  if (method == "PUT" && declared != "UNSIGNED-PAYLOAD") {
    if (crypto.sha256(req.body) != declared) {
      return {status: 400, body: "<Error><Code>XAmzContentSHA256Mismatch</Code></Error>"}
    }
  }
  return nil
}

async fn main() {
  let server = create()
  server.route("PUT", "/testbucket/:key", (req) => {
    let bad = handle(req, "PUT", req.params.key)
    if (bad != nil) {
      return bad
    }
    store[req.params.key] = req.body
    return {status: 200, headers: {etag: "\"d41d8cd98f00b204e9800998ecf8427e\""}, body: ""}
  })
  server.route("GET", "/testbucket/:key", (req) => {
    let bad = handle(req, "GET", req.params.key)
    if (bad != nil) {
      return bad
    }
    let v = store[req.params.key]
    if (v == nil) {
      return {status: 404, body: "<Error><Code>NoSuchKey</Code></Error>"}
    }
    return {status: 200, body: v}
  })

  // list-objects-v2 is GET /testbucket?list-type=2&...
  server.route("GET", "/testbucket", (req) => {
    let bad = handle(req, "GET", "")
    if (bad != nil) {
      return bad
    }
    let keys = array.sort(objectKeys(store))
    let contents = ""
    for (k of keys) {
      contents = contents + `<Contents><Key>${k}</Key><Size>${len(store[k])}</Size>` + `<ETag>&quot;etag-${k}&quot;</ETag></Contents>`
    }
    let body = `<?xml version="1.0"?><ListBucketResult>` + `<IsTruncated>false</IsTruncated>${contents}</ListBucketResult>`
    return {status: 200, headers: {"content-type": "application/xml"}, body: body}
  })
  let [port, berr] = await server.bind("127.0.0.1", 0)
  if (berr != nil) {
    print(`bind error: ${berr.message}`)
    return
  }

  // The client runs as a spawned task; main drives a bounded serve loop.
  let clientTask = task.spawn((async () => {
    let client = blob.client({endpoint: `http://127.0.0.1:${port}`, region: REGION, accessKey: ACCESS_KEY, secretKey: SECRET_KEY, bucket: "testbucket", pathStyle: true})
    let [etag, perr] = await client.put("greeting.txt", "Hello from std/blob!", {contentType: "text/plain"})
    let [_etag2, perr2] = await client.put("notes-day1.md", "# Day 1")
    let [body, gerr] = await client.get("greeting.txt")
    let keys = []
    for await (item in client.list({prefix: ""})) {
      keys = [...keys, item.key]
    }
    return {putErr: perr, getErr: gerr, body: gerr == nil ? utf8Decode(body)! : "<error>", keys: keys}
  })())

  // 4 signed requests: put, put, get, list (one page).
  let [_, serr] = await server.serve({maxRequests: 4})
  if (serr != nil) {
    print(`serve error: ${serr.message}`)
  }
  let r = await clientTask
  print(`put error: ${r.putErr}`)
  print(`get error: ${r.getErr}`)
  print(`round-tripped body: ${r.body}`)
  print(`listed keys: ${string.join(array.sort(r.keys), ", ")}`)
}

await main()
print("blob_basics ok")
