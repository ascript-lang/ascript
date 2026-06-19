// oauth_pkce_flow.as — a full OAuth2 Authorization-Code-with-PKCE flow against an
// in-script loopback identity provider, with token verification via a JWKS endpoint.
//
// The loopback IdP exposes two endpoints:
//   POST /token                    — exchanges the auth code (+ PKCE verifier) for an
//                                     ES256-signed access token (kid "ec-1")
//   GET  /.well-known/jwks.json    — publishes the matching EC public key as a JWK Set
//
// The client runs the real `std/oauth` + `std/jwt` paths: `oauth.pkce()` →
// `oauth.exchangeCode(...)` → `jwt.jwks(url)` → `cache.verify(token)`. The verifying
// public key comes ONLY from the JWKS endpoint (resolved by the token's `kid`), and
// the algorithm-confusion defense still holds (an EC JWK can never HS-verify).
//
// Deterministic + `maxRequests`-bounded → runs to completion (a four-mode corpus
// member). The PKCE verifier is random per run, so it is never printed; the issued
// token carries only FIXED claims (no `exp`), so the output is stable.

import { create } from "std/http/server"
import * as oauth from "std/oauth"
import * as jwt from "std/jwt"
import * as json from "std/json"
import * as task from "std/task"

const HOST = "127.0.0.1"
const ISSUER = "https://idp.example"

// A fixed EC P-256 keypair (test material). The IdP signs with the private key; the
// JWKS endpoint publishes the matching public JWK under kid "ec-1".
const EC_PRIV = "-----BEGIN PRIVATE KEY-----\nMIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg7NbeKWv4WlgMDSeU\nVBJ7MiglXi39mHXC4u/jo3+YQNKhRANCAAQNmZ0qT2mfxHA1nvzv5NHpISl3GjgE\nmQrprfaYVbL3ZfdXf/0FY1wsBuNQuljGPhOj28lQVQ/LAypg69kGQ5Wx\n-----END PRIVATE KEY-----\n"

const JWK_SET = {
  keys: [
    {
      kty: "EC",
      crv: "P-256",
      kid: "ec-1",
      alg: "ES256",
      use: "sig",
      x: "DZmdKk9pn8RwNZ787-TR6SEpdxo4BJkK6a32mFWy92U",
      y: "91d__QVjXCwG41C6WMY-E6PbyVBVD8sDKmDr2QZDlbE",
    },
  ],
}

let idp = create()

// The token endpoint: issue an ES256 access token (fixed claims, no exp).
idp.route("POST", "/token", (req) => {
  let signKey = jwt.ecPrivateKey(EC_PRIV)
  let [accessToken, signErr] = jwt.sign(
    { sub: "user-123", iss: ISSUER },
    signKey,
    { headers: { kid: "ec-1" } },
  )
  if (signErr != nil) {
    return { status: 500, body: `sign failed: ${signErr.message}` }
  }
  let [body, _] = json.stringify({
    access_token: accessToken,
    token_type: "Bearer",
  })
  return {
    status: 200,
    headers: { "content-type": "application/json" },
    body: body,
  }
})

// The JWKS endpoint: publish the public key set.
idp.route("GET", "/.well-known/jwks.json", (req) => {
  let [body, _] = json.stringify(JWK_SET)
  return {
    status: 200,
    headers: { "content-type": "application/json" },
    body: body,
  }
})

async fn runIdp() {
  // Two requests: POST /token (exchangeCode) + GET /jwks (jwt.jwks fetch).
  await idp.serve({ maxRequests: 2 })
}

async fn main() {
  let [port, berr] = await idp.bind(HOST, 0)
  if (berr != nil) {
    print(`bind failed: ${berr.message}`)
    return
  }
  let base = `http://${HOST}:${port}`
  let serving = task.spawn(runIdp())

  // 1. Generate a PKCE pair (verifier is random → never printed).
  let pkce = oauth.pkce()
  print(`pkce method: ${pkce.method}`)
  print(`verifier length: ${len(pkce.verifier)}`)
  print(`challenge length: ${len(pkce.challenge)}`)

  // 2. Exchange the (already-redeemed) authorization code for tokens.
  let [tokens, exErr] = await oauth.exchangeCode({
    tokenUrl: `${base}/token`,
    code: "auth-code-from-redirect",
    codeVerifier: pkce.verifier,
    clientId: "demo-client",
    redirectUri: `${base}/callback`,
  })
  if (exErr != nil) {
    print(`token exchange failed: ${exErr.message}`)
    await serving
    return
  }
  print(`token type: ${tokens.token_type}`)
  print(`got access token: ${tokens.access_token != nil}`)

  // 3. Verify the token via the JWKS endpoint (key resolved by the token's kid).
  let [cache, jwksErr] = await jwt.jwks(`${base}/.well-known/jwks.json`)
  if (jwksErr != nil) {
    print(`jwks fetch failed: ${jwksErr.message}`)
    await serving
    return
  }
  print(`jwks kids: ${cache.keys()}`)

  let [claims, verErr] = await cache.verify(tokens.access_token, {
    algs: ["ES256"],
    iss: ISSUER,
  })
  if (verErr != nil) {
    print(`verify failed: ${verErr.message}`)
  } else {
    print(`verified subject: ${claims.sub}`)
    print(`verified issuer: ${claims.iss}`)
  }

  await serving
}

await main()
print("oauth_pkce_flow ok")
