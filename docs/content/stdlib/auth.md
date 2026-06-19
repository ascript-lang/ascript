::: eyebrow Standard library

# Auth (JWT, OAuth2, sessions)

`std/jwt` issues and verifies **JSON Web Tokens** with a security model that makes the classic
**algorithm-confusion bypass structurally impossible**. Keys are *typed*: a key carries the
algorithm family it can ever be used with, and `jwt.verify` only accepts a token whose header
`alg` lies in the intersection of the key's family, the caller's allowlist, and (never) anything
the token itself claims about where to find a key.

`std/jwt` ships three signature families: the **HMAC** family — **HS256 / HS384 / HS512** (built
on `std/crypto`'s `hmac` + `sha2`) — and the **asymmetric** families **RS256** (RSA, RSASSA-PKCS1-v1_5
over SHA-256) and **ES256** (ECDSA on the P-256 curve over SHA-256). The asymmetric keys are typed
exactly like the HMAC key, so the algorithm-confusion defense below extends to them unchanged: an
RSA/EC **public** key can never HMAC-verify, and an RS256 key can never reach the ES256 path (or
vice versa).

```ascript
import * as jwt from "std/jwt"

let key = jwt.hmacKey("a-strong-shared-secret")

let [token, signErr] = jwt.sign({ sub: "alice", role: "admin" }, key, { expiresIn: 3600 })
let [claims, verifyErr] = jwt.verify(token, key, { algs: ["HS256"], leeway: 30 })
if verifyErr != nil {
  print("rejected:", verifyErr.message)   // auth failure is a Tier-1 [value, err]
} else {
  print("subject:", claims.sub)
}
```

## Errors are values, not panics

A failed verification is **never** a panic — it is a Tier-1 `[nil, err]` pair, because
authentication failure is ordinary control flow (an expired token, a tampered signature, a
disallowed algorithm). Passing a value that is not a key where a key is required *is* a Tier-2
panic, because that is a programming error.

## Typed keys kill algorithm confusion

A key is a tagged object — `jwt.hmacKey(secret)` produces `{ __jwtkey: "hmac", secret }` — and the
key's kind fixes the algorithm set it can ever participate in:

| Key kind | Constructor | Algorithms |
| --- | --- | --- |
| `hmac` | `jwt.hmacKey(secret)` | HS256, HS384, HS512 |
| `rsa-public` | `jwt.rsaPublicKey(pem)` | RS256 (verify) |
| `rsa-private` | `jwt.rsaPrivateKey(pem)` | RS256 (sign + verify) |
| `ec-public` | `jwt.ecPublicKey(pem)` | ES256 (verify) |
| `ec-private` | `jwt.ecPrivateKey(pem)` | ES256 (sign + verify) |

`jwt.verify` computes `allowed = algorithms(key) ∩ (opts.algs or algorithms(key))` and rejects any
token whose header `alg` is not in `allowed`. Three consequences follow directly:

- **`alg: "none"` is rejected unconditionally** — in any casing (`none`, `None`, `NONE`) — *before*
  any key dispatch, so a signature-stripped token can never be accepted.
- An HMAC key can only ever HS-verify; once asymmetric keys land, an RSA/EC **public** key can never
  HMAC-verify, so the public-key-as-HMAC-secret confusion is unrepresentable.
- The `kid`, `jku`, `jwk`, and `x5u` header fields are **never read** — keys come *only* from the
  `key` argument you pass. A token can advertise any key location it likes; it is ignored.

The signature is verified in **constant time** (the underlying MAC's `verify_slice`), and
authenticity is checked **before** any claim (`exp`/`nbf`/`iss`/`aud`), so a claim failure never
leaks before the token is proven genuine.

---

### `jwt.hmacKey(secret)`

Builds a typed HMAC key usable for HS256/HS384/HS512. `secret` is a `string` or `bytes`.

### `jwt.rsaPublicKey(pem)`

Builds a typed RSA **public** key (RS256, verify-only) from a `pem` string (SPKI or PKCS#1). The
PEM is validated at construction — a malformed or non-RSA PEM returns a Tier-1 `[nil, err]`.

### `jwt.rsaPrivateKey(pem)`

Builds a typed RSA **private** key (RS256, sign + verify) from a `pem` string (PKCS#8 or PKCS#1).
Validated at construction.

### `jwt.ecPublicKey(pem)`

Builds a typed EC **public** key (ES256, on the P-256 curve, verify-only) from a `pem` string
(SPKI). Validated at construction — a non-EC or non-P-256 PEM is a Tier-1 error.

### `jwt.ecPrivateKey(pem)`

Builds a typed EC **private** key (ES256, sign + verify) from a `pem` string (PKCS#8 or SEC1).
Validated at construction.

## RS256 and ES256 (asymmetric signing)

```ascript
import * as jwt from "std/jwt"

let signKey = jwt.rsaPrivateKey(privatePem)   // or jwt.ecPrivateKey(...)
let verifyKey = jwt.rsaPublicKey(publicPem)   // or jwt.ecPublicKey(...)

let [token, _] = jwt.sign({ sub: "alice" }, signKey)              // alg defaults to RS256 / ES256
let [claims, err] = jwt.verify(token, verifyKey, { algs: ["RS256"] })
```

Asymmetric keys **store the PEM text** in the key object (the `__jwtkey` tag shows the kind; the
material is an ordinary field) and re-parse it per operation — keys are not a hot path, and this
keeps a key both sendable across the worker airlock and printable-safe. Treat the PEM string as you
would any secret.

::: warning
**ES256 signatures are fixed-width JOSE (r‖s), never DER.** Per RFC 7518 §3.4 the ECDSA signature is
the 64-byte concatenation of `r` and `s`, *not* the variable-length ASN.1/DER encoding. `jwt.sign`
emits the fixed-width form and `jwt.verify` accepts only the fixed-width form — a DER-encoded
signature (`0x30…`) is rejected by construction.
:::

### `jwt.sign(claims, key, opts?)`

Signs `claims` (an object) into a compact JWT string with `key`. Returns `[token, err]`. Claim
order follows the object's insertion order. `opts`:

- `alg?: string` — one of `HS256`/`HS384`/`HS512`/`RS256`/`ES256`; must be valid for the key kind.
  Defaults to `HS256` for an HMAC key, or the key kind's sole algorithm (`RS256`/`ES256`) otherwise.
- `headers?: object` — extra protected headers (e.g. `kid`); cannot override `alg`/`typ`.
- `expiresIn?: number` — seconds; sets `exp` from the (deterministic-seam) clock.

### `jwt.verify(token, key, opts?)`

Verifies `token` against `key` and returns `[claims, err]`. `opts`:

- `algs?: array<string>` — an allowlist, **intersected** with the key kind's algorithm set.
- `iss?: string` / `aud?: string` — expected issuer / audience (a mismatch fails).
- `leeway?: number` — clock-skew tolerance in seconds for `exp`/`nbf`.
- `clock?: number` — override the current time (ms epoch) for testing.

::: warning
### `jwt.decode(token)`

Decodes a token into `{ header, claims, signature, verified: false }` **without verifying the
signature** — for routing and debugging only. The result's `verified: false` field testifies that
nothing was checked. Never trust `jwt.decode` output for authentication; use `jwt.verify`.
:::

## Verifying with a JWKS endpoint

When an identity provider publishes its public keys at a JWKS URL, `jwt.jwks(url)` fetches the JWK
Set once, caches it, and returns a handle whose `verify` resolves each token's `kid` header to the
matching cached key. The fetch reuses AScript's **pooled HTTP client** — there is no second network
stack. `jwt.jwks` is the only network-touching `std/jwt` function, so it is gated by the `net`
capability (`--deny net` / `--sandbox` block it, while `jwt.sign`/`jwt.verify` stay available as
pure crypto).

```ascript
import * as jwt from "std/jwt"

let [keys, fetchErr] = jwt.jwks("https://issuer.example/.well-known/jwks.json")
if fetchErr != nil { exit(1) }

let [claims, err] = keys.verify(token, { algs: ["RS256"], iss: "https://issuer.example" })
```

The same algorithm-confusion defense applies: a JWKS key is converted to a typed `rsa-public` /
`ec-public` key, so a JWKS RSA key can never verify an `HS256` token. Unsupported JWK types
(`oct`, non-P-256 curves) are skipped; the usable RSA/EC keys remain available.

### `jwt.jwks(url, opts?)`

Fetches a JWK Set from `url` and returns `[cache, err]`. `opts.ttlMs?` sets the cache lifetime
(default 5 minutes). The returned cache handle has:

- `cache.verify(token, opts?)` — resolve the token's `kid`, then verify through the same path as
  `jwt.verify` (same `opts`). On an unknown `kid` or an expired TTL the cache **refetches exactly
  once** before resolving; a verify within the TTL never refetches. Returns `[claims, err]`.
- `cache.keys()` — the kids currently cached, as `array<string>`.
- `cache.close()` — drop the cached keys.

## OAuth2 + PKCE

`std/oauth` is a small OAuth2 client over the **same pooled HTTP client**. The whole module is
`net`-gated. A non-2xx token response is a Tier-1 `[nil, err]` whose error carries the provider's
response body (so you see `{ error, error_description }`); a wrong argument type is a Tier-2 panic.

```ascript
import * as oauth from "std/oauth"

// 1. Generate a PKCE pair, send `pkce.challenge` on the authorization request.
let pkce = oauth.pkce()   // { verifier, challenge, method: "S256" }

// 2. After the redirect, exchange the code (with the verifier) for tokens.
let [tokens, err] = oauth.exchangeCode({
  tokenUrl: "https://issuer.example/token",
  code: authCode,
  codeVerifier: pkce.verifier,
  clientId: "my-client",
  clientSecret: "s3cr3t",          // omit for a public client
  redirectUri: "https://app.example/callback",
})
```

### `oauth.pkce()`

Returns `{ verifier, challenge, method: "S256" }`. The `verifier` is a 43-character base64url string
(32 bytes of entropy, via the determinism seam so it is reproducible under record/replay); the
`challenge` is `base64url(sha256(verifier))` per RFC 7636.

### `oauth.exchangeCode(opts)`

Exchanges an authorization `code` for tokens (`grant_type=authorization_code`). A `clientSecret`
selects HTTP Basic client authentication; otherwise the `clientId` rides in the form (public
client). `opts`: `tokenUrl`, `code`, `codeVerifier`, `clientId`, `clientSecret?`, `redirectUri?`.
Returns `[tokens, err]`.

### `oauth.clientCredentials(opts)`

The `client_credentials` grant (machine-to-machine, Basic auth). `opts`: `tokenUrl`, `clientId`,
`clientSecret`, `scope?`. Returns `[tokens, err]`.

### `oauth.refresh(opts)`

Exchanges a `refreshToken` for a fresh access token (`grant_type=refresh_token`). `opts`:
`tokenUrl`, `refreshToken`, `clientId`, `clientSecret?`. Returns `[tokens, err]`.

### `oauth.discover(issuer)`

Fetches an issuer's OpenID Connect metadata from `<issuer>/.well-known/openid-configuration` and
returns `[metadata, err]`.
