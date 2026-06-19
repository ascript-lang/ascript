::: eyebrow Standard library

# Object storage (S3-compatible)

`std/blob` is a small S3-compatible object-storage client. It talks the S3 REST API
over the shared pooled HTTP client (the same one `std/net/http` uses — there is no
second HTTP stack) and signs every request with **AWS Signature Version 4 (SigV4)**.
It works against Amazon S3, Cloudflare R2, MinIO, Backblaze B2, and any other
S3-compatible endpoint.

Every operation is **Tier-1**: it returns a `[value, err]` pair. An S3 error response
decodes to a structured `{code, message, status}` error; a network failure or a
malformed XML body is a clean error, never a panic. Misuse — a non-object client
config, a non-array `list` opts, or a configured multipart part size below the 5 MiB
floor — is a **Tier-2** panic.

The whole module is `net`-gated, **including `presign`**: a presigned URL is minted
from the secret key and carries that secret's authority, so it is gated alongside the
rest of the secret-handling surface. Under `--sandbox` or `--deny net`, every `blob`
call (including `presign`) is denied before any work happens.

```ascript
import * as blob from "std/blob"
import * as encoding from "std/encoding"

let client = blob.client({
  endpoint: "https://s3.us-east-1.amazonaws.com",
  region: "us-east-1",
  accessKey: env.get("AWS_ACCESS_KEY_ID")!,
  secretKey: env.get("AWS_SECRET_ACCESS_KEY")!,
  bucket: "my-bucket",
})

let [etag, perr] = client.put("notes/hello.txt", "hello world", { contentType: "text/plain" })
if perr != nil { print("upload failed:", perr.message); exit(1) }

let [data, gerr] = client.get("notes/hello.txt")
if gerr != nil { print("download failed:", gerr.message); exit(1) }
print(encoding.utf8Decode(data)[0])
```

## `blob.client(config) -> client`

`config` is an object:

| Field | Type | Notes |
|-------|------|-------|
| `endpoint` | `string` | **required** — the absolute endpoint URL (`scheme://host[:port]`). |
| `region` | `string` | **required** — the AWS region (e.g. `"us-east-1"`); R2 accepts `"auto"`. |
| `accessKey` | `string` | **required** — the access key id. |
| `secretKey` | `string` | **required** — the secret access key (used for SigV4 signing). |
| `sessionToken` | `string` | optional STS session token (sent as `x-amz-security-token`). |
| `bucket` | `string` | optional default bucket (overridable per-call via `opts.bucket`). |
| `pathStyle` | `bool` | optional addressing style. Defaults to path-style (`endpoint/bucket/key`) for non-AWS endpoints and to virtual-host (`bucket.host/key`) for `amazonaws.com`. |

The returned `client` is a handle exposing the methods below. It holds **config only**
— no open socket — so each operation makes its own freshly-signed request.

### Addressing styles

- **Path-style** (`pathStyle: true`) — the bucket is the first path segment:
  `https://host/my-bucket/key`. The default for non-AWS endpoints (MinIO, etc.).
- **Virtual-host** (`pathStyle: false`) — the bucket is a host prefix:
  `https://my-bucket.host/key`. The default for `amazonaws.com`, and the form R2 uses.

The signed `host` header always matches the host the request actually connects to.

## `client.put(key, data, opts?) -> [etag, err]`

Uploads an object. `data` is a `string` or `bytes`. `opts` is `{contentType?,
metadata?, bucket?}`; `metadata` is an object whose string entries become
`x-amz-meta-*` headers. Returns the object's ETag (quotes stripped).

## `client.get(key, opts?) -> [bytes, err]`

Downloads an object as `bytes`. `opts` is `{bucket?, range?}`; `range` is a
`[start, end]` byte pair that becomes a `Range: bytes=start-end` request (a partial
`206` response).

## `client.head(key, opts?) -> [meta, err]`

Fetches object metadata without the body. `meta` is
`{size, etag, contentType, lastModified, metadata}` where `metadata` is an object of
the `x-amz-meta-*` headers (keys without the prefix). `opts` is `{bucket?}`.

## `client.delete(key, opts?) -> [nil, err]`

Deletes an object. `opts` is `{bucket?}`. Returns `[nil, nil]` on success.

## `client.list(opts?) -> generator`

Returns a **lazy** generator that yields one `{key, size, etag, lastModified}` object
per stored object. It paginates automatically across S3 continuation tokens — the next
page is fetched only when iteration crosses past the current page's entries. `opts` is
`{prefix?, delimiter?, bucket?, pageSize?}`.

```ascript
for await (obj in client.list({ prefix: "notes/" })) {
  print(obj.key, obj.size)
}
```

## `client.presign(method, key, opts?) -> [url, err]`

Mints a presigned URL that grants temporary access to perform `method` (e.g. `"GET"`,
`"PUT"`) on `key` — no network round-trip. `opts` is `{expires?, bucket?,
contentType?}`; `expires` is in seconds (default `900`). The URL embeds the SigV4
query parameters (`X-Amz-Algorithm`, `X-Amz-Credential`, `X-Amz-Signature`, …). This
call is `net`-gated because the URL carries the secret key's authority.

## `client.putMultipart(key, source, opts?) -> [etag, err]`

Uploads a large object in parts: it initiates a multipart upload, uploads each chunk
as a part (in order), then completes the upload. On **any** part error it issues an
`AbortMultipartUpload` so no orphaned upload is left behind. `source` is an array of
`bytes`/`string` chunks. A configured `opts.partSize` below 5 MiB (S3's floor for
non-final parts) is a Tier-2 error. Returns the final object ETag.

## Errors

An S3 error response surfaces as a structured error value:

| Field | Type | Notes |
|-------|------|-------|
| `code` | `string` | the S3 error code (e.g. `"AccessDenied"`, `"NoSuchKey"`). |
| `message` | `string` | the human-readable message (or a snippet of the raw body). |
| `status` | `int` | the HTTP status code. |

A malformed XML body still yields a clean error carrying the status — never a panic.
