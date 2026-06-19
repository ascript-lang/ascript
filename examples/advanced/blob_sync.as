// blob_sync.as — a multipart upload over an in-script S3 stub.
//
// blob.putMultipart streams a large object as parts: InitiateMultipartUpload
// (POST ?uploads) → UploadPart per chunk (PUT ?partNumber=N&uploadId=...) →
// CompleteMultipartUpload (POST ?uploadId=...). Non-final parts must be ≥ 5 MiB
// (the S3 floor); the final part may be smaller. On any error the client issues
// AbortMultipartUpload so no orphaned upload is left behind.
//
// The in-script stub recomputes the SigV4 signature for every request (the same
// server-side verification as blob_basics) and drives the multipart state
// machine. The client runs as a spawned task; the main task serves a bounded
// number of requests so the program runs to completion.
import { create } from "std/http/server"
import * as blob from "std/blob"
import * as string from "std/string"
import * as crypto from "std/crypto"
import * as object from "std/object"
import * as array from "std/array"
import { hexDecode } from "std/encoding"
import * as task from "std/task"

const REGION = "us-east-1"
const SECRET_KEY = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
const SERVICE = "s3"
const UPLOAD_ID = "test-upload-0001"

// ── SigV4 recomputation (server side) ──────────────────────────────────────────
fn hmacBytes(keyBytes, msg) {
  return hexDecode(crypto.hmacSha256(keyBytes, msg))!
}
fn signingKey(secret, date, region, service) {
  let kDate = hmacBytes(`AWS4${secret}`, date)
  let kRegion = hmacBytes(kDate, region)
  let kService = hmacBytes(kRegion, service)
  return hmacBytes(kService, "aws4_request")
}
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
fn sliceBetween(s, start, end) {
  let i = string.find(s, start)
  if (i < 0) {
    return nil
  }
  let rest = string.slice(s, i + len(start), len(s))
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
fn canonicalQuery(queryObj) {
  let pairs = []
  for (k of object.keys(queryObj)) {
    pairs = [...pairs, `${k}=${queryObj[k]}`]
  }
  return string.join(array.sort(pairs), "&")
}
fn verifySig(method, path, query, headers, authHeader) {
  let credPart = sliceBetween(authHeader, "Credential=", ",")
  let signedHeaders = sliceBetween(authHeader, "SignedHeaders=", ",")
  let theirSig = sliceAfter(authHeader, "Signature=")
  if (credPart == nil || signedHeaders == nil || theirSig == nil) {
    return false
  }
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
  let names = string.split(signedHeaders, ";")
  let canonHeaders = ""
  for (name of names) {
    let val = headers[name]
    if (val == nil) {
      return false
    }
    canonHeaders = canonHeaders + `${name}:${canonHeaderValue(val)}\n`
  }
  let canonReq = `${method}\n${path}\n${query}\n${canonHeaders}\n${signedHeaders}\n${payloadHash}`
  let scope = `${date}/${REGION}/${SERVICE}/aws4_request`
  let stringToSign = `AWS4-HMAC-SHA256\n${amzDate}\n${scope}\n${crypto.sha256(canonReq)}`
  let key = signingKey(SECRET_KEY, date, REGION, SERVICE)
  let expected = crypto.hmacSha256(key, stringToSign)
  return crypto.timingSafeEqual(expected, theirSig)
}

// Guard: verify the signature; return a 403 response on mismatch, else nil.
fn guard(req, method) {
  let auth = req.headers["authorization"] ?? ""
  let cq = canonicalQuery(req.query)
  if (!verifySig(method, req.path, cq, req.headers, auth)) {
    return {status: 403, body: "<Error><Code>SignatureDoesNotMatch</Code></Error>"}
  }
  return nil
}

async fn main() {
  let server = create()
  let state = {parts: 0, partBytes: 0, completed: false}

  // POST /bucket/key — either ?uploads (init) or ?uploadId=... (complete).
  server.route("POST", "/bucket/:key", (req) => {
    let bad = guard(req, "POST")
    if (bad != nil) {
      return bad
    }
    if (req.query["uploads"] != nil) {
      // InitiateMultipartUpload.
      let body = `<?xml version="1.0"?><InitiateMultipartUploadResult>` + `<Bucket>bucket</Bucket><Key>${req.params.key}</Key>` + `<UploadId>${UPLOAD_ID}</UploadId></InitiateMultipartUploadResult>`
      return {status: 200, headers: {"content-type": "application/xml"}, body: body}
    }
    // CompleteMultipartUpload.
    state.completed = true
    let body = `<?xml version="1.0"?><CompleteMultipartUploadResult>` + `<Location>http://127.0.0.1/bucket/${req.params.key}</Location>` + `<Bucket>bucket</Bucket><Key>${req.params.key}</Key>` + `<ETag>&quot;multipart-final-etag-${state.parts}&quot;</ETag>` + `</CompleteMultipartUploadResult>`
    return {status: 200, headers: {"content-type": "application/xml"}, body: body}
  })

  // PUT /bucket/key?partNumber=N&uploadId=... — UploadPart.
  server.route("PUT", "/bucket/:key", (req) => {
    let bad = guard(req, "PUT")
    if (bad != nil) {
      return bad
    }
    state.parts = state.parts + 1
    state.partBytes = state.partBytes + len(req.body)
    // Each part gets a distinct ETag (S3 returns it in the ETag response header).
    let etag = crypto.sha256(req.body)
    return {status: 200, headers: {etag: `"${etag}"`}, body: ""}
  })
  let [port, berr] = await server.bind("127.0.0.1", 0)
  if (berr != nil) {
    print(`bind error: ${berr.message}`)
    return
  }

  // Two parts: a 5 MiB non-final part + a small final part.
  let bigPart = string.repeat("0123456789", 524288) // exactly 5 MiB
  let lastPart = "the tail end of the object"
  let clientTask = task.spawn((async () => {
    let client = blob.client({endpoint: `http://127.0.0.1:${port}`, region: REGION, accessKey: "AKIAIOSFODNN7EXAMPLE", secretKey: SECRET_KEY, bucket: "bucket", pathStyle: true})
    let [etag, err] = await client.putMultipart("big-object.bin", [bigPart, lastPart], {contentType: "application/octet-stream"})
    return {etag: etag, err: err}
  })())

  // 4 requests: init + 2 parts + complete.
  let [_, serr] = await server.serve({maxRequests: 4})
  if (serr != nil) {
    print(`serve error: ${serr.message}`)
  }
  let r = await clientTask
  print(`upload error: ${r.err}`)
  print(`parts uploaded: ${state.parts}`)
  print(`total part bytes: ${state.partBytes}`)
  print(`completed: ${state.completed}`)
  print(`final etag present: ${r.etag != nil && len(r.etag) > 0}`)
}

await main()
print("blob_sync ok")
