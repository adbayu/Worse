# Remote relay (Remote Control without a tunnel)

Goal: like Antigravity's Remote Control, the user switches **Enable Remote Control** on and scans a QR code.
No tunnel binary, no open port, no Cloudflare API key in the app. The relay is a Cloudflare Worker with one
Durable Object per desktop (`relay/`), served at `https://worse.digidaw.site/<device-id>`. Forks can deploy
their own relay and change the relay URL in Settings › Remote › Advanced.

```
phone (browser / APK) ──wss──►  worse.digidaw.site  ◄──wss── Harness desktop (outbound only)
                                 Worker + Durable Object
                                 (forwards ciphertext only)
```

The relay never sees plaintext: phone and desktop encrypt end to end with a secret that only travels in the
URL fragment (`#k=…`), which browsers never send to a server.

## 1. Identity

- The desktop keeps an Ed25519 key pair in `<HARNESS_HOME>/relay.json` (never logged, never sent).
- `device-id` = first 16 chars of lowercase RFC 4648 base32 (no padding) of `SHA-256(public key)`.
  Nobody can claim someone else's id without finding a matching key (80 bits).
- The link secret `k` = 32 random bytes, base64url, also in `relay.json`. **Reset link** makes a new `k`;
  every phone has to scan again.
- QR / copy link: `https://worse.digidaw.site/<device-id>#k=<k>` (QR adds `&p=<pair token>` so one scan also
  pairs; see §4).

## 2. Relay protocol (Worker ⇄ endpoints)

| Endpoint | Who | Notes |
| --- | --- | --- |
| `GET /` | anyone | landing page |
| `GET /<id>` | phone | the web client (static assets) |
| `GET /<id>/host?pub=&ts=&sig=` (WebSocket) | desktop | `sig` = Ed25519 over `worse-relay/1 host <id> <ts>`; `ts` unix seconds within ±300 s; `id` must match `pub`. A new host replaces the old one. |
| `GET /<id>/client` (WebSocket) | phone | anyone may connect; everything is E2E encrypted, the host drops what it cannot decrypt |
| `GET /healthz` | anyone | `{"ok":true}` |

Frames:

- phone ⇄ relay: **binary** = opaque payload. **Text** from the relay: `{"t":"online"}`, `{"t":"offline"}`,
  `{"t":"error","m":"…"}` (sent before the relay closes the socket, e.g. `closed by the desktop`).
- desktop ⇄ relay: **binary** = `cid` (u32 big endian) + payload, in both directions. **Text** from the relay:
  `{"t":"open","c":n}`, `{"t":"close","c":n}`, `{"t":"replaced"}` (another desktop signed in with the same key;
  the old socket closes with 4000); text from the desktop: `{"t":"kick","c":n}` (closes that phone with 4001).
- Text `ping` is answered with `pong` by the runtime (WebSocket auto-response), without waking the object.

Limits: 8 phones per device, 1 MiB per frame (endpoints chunk at 256 KiB), 1200 frames per minute per
connection. Free-plan budget: idle sockets hibernate (no duration), incoming WebSocket messages bill 20:1.

## 3. End-to-end encryption (phone ⇄ desktop)

1. Phone sends `0x01 ‖ salt_c` (16 random bytes); desktop answers `0x01 ‖ salt_h`.
2. Keys: `HKDF-SHA256(ikm = k, salt = salt_c ‖ salt_h, info = "worse-relay/1 c2h" | "worse-relay/1 h2c")`,
   32 bytes each.
3. Data frame: `0x02 ‖ seq (u64 BE) ‖ AES-256-GCM(key, nonce = 0x00000000 ‖ seq, aad = 0x02 ‖ seq)`.
   `seq` starts at 0 per direction and must grow by exactly 1, so replayed or reordered frames are rejected.
   Fresh salts per connection make frames from another connection useless.
4. Plaintext = `flags (u8, bit 0 = more chunks follow)` ‖ chunk. Chunks join into one UTF-8 JSON message
   (max 32 MiB).

## 4. Inner protocol (JSON, after decryption)

The desktop bridges messages to the existing loopback server of `harness-remote`, so routes, rpc allowlist,
device tokens and pairing stay exactly as they are.

| Phone → desktop | Desktop → phone |
| --- | --- |
| `{"t":"req","id":n,"method":"POST","path":"/api/v1/rpc","headers":{…},"body":"<base64>"}` | `{"t":"res","id":n,"status":200,"headers":{…},"body":"<base64>"}` |
| `{"t":"ws-open","id":n,"path":"/api/v1/events","headers":{…}}` | `{"t":"ws","id":n,"data":"…"}` / `{"t":"ws-close","id":n}` |
| `{"t":"ws","id":n,"data":"…"}` / `{"t":"ws-close","id":n}` | |

The phone authenticates with `Authorization: Bearer <device token>` (no cookies across the relay). The QR's
`p` is the existing single-use pair offer token; the phone redeems it through `POST /api/v1/pair`, which mints
a **new random device token** (the QR token is dead after use), and keeps that token (read from the relayed
`Set-Cookie`) and `k` in local storage (`crates/harness-remote/src/assets/relay.js`).

Security rules: each relay phone connection gets its own rate-limit key (a random `100.64.0.0/10` address),
so one phone's failed attempts never lock the others out; a phone may only revoke itself
(`/api/v1/devices/revoke` with another id → 403); **Reset link** also unpairs every device; a device pushed out
by the 8-device cap is disconnected at once. When a new desktop replaces a connected one, the relay sends phones
`{"t":"error","m":"desktop reconnected"}` and closes them with 4002, so they reconnect and handshake again.

## 5. Phases

1. `relay/` Worker + Durable Object, tests against `wrangler dev` (`relay/test/run.mjs`), phone transport
   against the Rust vectors (`relay/test/client.mjs`); `npm test` runs both.
2. Desktop: `relay.json`, relay client with reconnect and E2E, bridge to the loopback server; Remote page like
   Antigravity (toggle, Device Name, QR, "Open in Remote Control" popup, copy link, Reset link, Advanced).
3. Web client / APK: relay transport + E2E in `crates/harness-remote/src/assets`, synced into `relay/public`.
4. Deploy (the owner): `wrangler login`, `wrangler deploy`, Custom Domain `worse.digidaw.site`.
