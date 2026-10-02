# tapbridge-relay (Cloudflare Worker)

FCM wake relay for tapbridge: the desktop POSTs `{token, host, hint}` to
`POST /v1/wake`; the worker signs an RS256 JWT against the Firebase service
account, caches the OAuth access token, and sends a data-only, HIGH-priority,
60 s TTL FCM HTTP v1 message. Rate limited to 10 requests/min per token
(key = SHA-256 of the token). Contract: `202 {ok:true}` accepted, `400`
malformed, `405` wrong method, `404` other paths, `410 {error:"unregistered"}`
token is dead/invalid (desktop tells the user to reopen the phone app),
`429` rate limited, `502` upstream failure. No CORS — desktop client only.
Tokens, hints, and secrets are never logged (only status codes and an
8-hex-char token-hash prefix).

## Deploy (one time, per environment)

Requires node/npm (`wrangler` is a devDependency here) and a Cloudflare account.

1. **Log in to Cloudflare** (opens a browser; the deploy step itself is not run
   automatically — you do it):
   ```
   cd relay
   npx wrangler login
   ```

2. **Create the Firebase service-account key**
   - Firebase console → your project → **Project settings → Service accounts →
     Generate new private key** → download the JSON file
     (e.g. `tapbridge-xxxx-firebase-adminsdk-xxxx.json`).

3. **Upload it as a secret, then delete the JSON file**
   ```
   npx wrangler secret put FCM_SERVICE_ACCOUNT < path\to\key.json
   del path\to\key.json
   ```
   (The secret name is bound in `src/worker.ts` / `wrangler.toml`; the project
   id is read from inside the JSON, so nothing else is configured.)

4. **Deploy**
   ```
   npx wrangler deploy
   ```

## Tests / verification

```
npm install
npm test                 # vitest, plain node runtime with mocked fetch/RateLimit
npm run check            # tsc --noEmit
npx wrangler deploy --dry-run --outdir dist
```

Platform note (Windows ARM64): `workerd` ships no win32-arm64 build, so
`@cloudflare/vitest-pool-workers` (workerd-backed test pool) cannot run here —
tests use plain vitest with mocked `fetch` and an in-memory RateLimit mock, and
`relay/.npmrc` sets `ignore-scripts=true` so `npm install` succeeds. The x64
workerd binary (`@cloudflare/workerd-windows-64`) is vendored into
`node_modules/@cloudflare/` manually to let `wrangler --dry-run`/`deploy` run
under x64 emulation; on x64 hosts a plain `npm install` is enough.

## Pointing the desktop build at the relay

The relay URL is built into `tapbridge.exe` at compile time. Set
`TAPBRIDGE_RELAY_URL` to the deployed worker origin, then build the
installers:

```
$env:TAPBRIDGE_RELAY_URL = 'https://tapbridge-relay.<your-subdomain>.workers.dev'
powershell -File scripts\build-desktop.ps1 -Arch both
```

An installed copy can point at a different relay with one line in
`%LOCALAPPDATA%\tapbridge\relay.txt`. The desktop sends
`POST <relay>/v1/wake` with `{"token": ..., "host": ..., "hint": ...}`.
