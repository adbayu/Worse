#!/usr/bin/env node
// Phone relay transport (crates/harness-remote/src/assets/relay.js) against the protocol of
// docs/plans/remote-relay.md §3–4, with a fake relay socket. The test's own crypto is first checked against
// the Rust vectors (crates/harness-remote/src/relay/crypto.rs `matches_webcrypto_vectors`), so passing here
// means the phone and the desktop agree byte for byte.
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const relayJs = path.resolve(here, "..", "..", "crates", "harness-remote", "src", "assets", "relay.js");
const enc = new TextEncoder();
const dec = new TextDecoder();
const range = (n, start) => Uint8Array.from({ length: n }, (_, i) => start + i);
const hex = (b) => Buffer.from(b).toString("hex");
const b64url = (b) => Buffer.from(b).toString("base64url");

// ---- reference crypto (spec §3) ----
async function key(secret, salt, info) {
  const base = await crypto.subtle.importKey("raw", secret, "HKDF", false, ["deriveKey"]);
  return crypto.subtle.deriveKey(
    { name: "HKDF", hash: "SHA-256", salt, info: enc.encode(info) },
    base,
    { name: "AES-GCM", length: 256 },
    false,
    ["encrypt", "decrypt"],
  );
}
const head = (seq) => {
  const h = new Uint8Array(9);
  h[0] = 2;
  new DataView(h.buffer).setBigUint64(1, BigInt(seq));
  return h;
};
const iv = (seq) => {
  const n = new Uint8Array(12);
  new DataView(n.buffer).setBigUint64(4, BigInt(seq));
  return n;
};
async function seal(k, seq, plain) {
  const h = head(seq);
  const body = new Uint8Array(await crypto.subtle.encrypt({ name: "AES-GCM", iv: iv(seq), additionalData: h }, k, plain));
  return Uint8Array.from([...h, ...body]);
}
async function open(k, seq, frame) {
  assert.equal(frame[0], 2);
  assert.equal(Number(new DataView(frame.buffer, frame.byteOffset).getBigUint64(1)), seq);
  return new Uint8Array(
    await crypto.subtle.decrypt({ name: "AES-GCM", iv: iv(seq), additionalData: head(seq) }, k, frame.subarray(9)),
  );
}

const secret = range(32, 0);
const saltC = range(16, 32);
const saltH = range(16, 48);
const salt = Uint8Array.from([...saltC, ...saltH]);
const c2h = await key(secret, salt, "worse-relay/1 c2h");
const h2c = await key(secret, salt, "worse-relay/1 h2c");
assert.equal(
  hex(await seal(c2h, 0, enc.encode("\0hello"))),
  "02000000000000000084458669264a93811c3ce0ccc5a1a3629daee512144f",
  "reference crypto matches the Rust vector (phone side)",
);
assert.equal(
  hex(await seal(h2c, 0, enc.encode("\0hello"))),
  "02000000000000000017ccc819c63375d3fc1dca464d28f4fba39c3b2246d6",
  "reference crypto matches the Rust vector (desktop side)",
);

// ---- browser globals for relay.js ----
const sockets = [];
class FakeSocket extends EventTarget {
  constructor(url) {
    super();
    this.url = url;
    this.sent = [];
    this.waiters = [];
    sockets.push(this);
  }
  send(data) {
    const item = typeof data === "string" ? data : new Uint8Array(data);
    const w = this.waiters.shift();
    if (w) w(item);
    else this.sent.push(item);
  }
  next() {
    return this.sent.length ? Promise.resolve(this.sent.shift()) : new Promise((r) => this.waiters.push(r));
  }
  close() {
    this.dispatchEvent(new Event("close"));
  }
  text(value) {
    this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify(value) }));
  }
  binary(bytes) {
    this.dispatchEvent(new MessageEvent("message", { data: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.length) }));
  }
}
const storage = new Map();
let address = "";
globalThis.location = {
  pathname: "/abcdefghijklmnop",
  hash: `#k=${b64url(secret)}&p=PAIRTOKEN&n=Studio`,
  protocol: "https:",
  host: "relay.example",
};
globalThis.history = { replaceState: (_s, _t, url) => (address = url) };
globalThis.localStorage = {
  getItem: (k) => (storage.has(k) ? storage.get(k) : null),
  setItem: (k, v) => storage.set(k, String(v)),
};
globalThis.WebSocket = FakeSocket;
const realRandom = crypto.getRandomValues.bind(crypto);
crypto.getRandomValues = (buf) => (buf.length === 16 ? (buf.set(saltC), buf) : realRandom(buf));

const relay = await import(pathToFileURL(relayJs).href);

let failed = 0;
async function step(name, fn) {
  try {
    await fn();
    console.log(`ok   ${name}`);
  } catch (error) {
    failed++;
    console.log(`FAIL ${name}\n     ${error.stack ?? error}`);
  }
}

await step("reads the link fragment and clears it", async () => {
  assert.equal(relay.relayActive, true);
  assert.equal(relay.relayMissingSecret(), false);
  assert.equal(relay.takePairToken(), "PAIRTOKEN");
  assert.equal(relay.takePairToken(), "");
  assert.equal(address, "/abcdefghijklmnop");
  assert.equal(JSON.parse(storage.get("worse-relay:abcdefghijklmnop")).k, b64url(secret));
});

let ws;
let sendSeq = 0;
let recvSeq = 0;
async function nextMessage() {
  const plain = await open(c2h, recvSeq++, await ws.next());
  assert.equal(plain[0], 0, "single chunk");
  return JSON.parse(dec.decode(plain.subarray(1)));
}
async function reply(message) {
  ws.binary(await seal(h2c, sendSeq++, Uint8Array.from([0, ...enc.encode(JSON.stringify(message))])));
}

await step("handshake, encrypted pair request, token kept from Set-Cookie", async () => {
  const pending = relay.relayFetch("/api/v1/pair", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ token: "PAIRTOKEN" }),
  });
  await new Promise((r) => setTimeout(r, 0));
  ws = sockets[0];
  assert.equal(ws.url, "wss://relay.example/abcdefghijklmnop/client");
  ws.text({ t: "online" });
  const hello = await ws.next();
  assert.deepEqual([...hello], [1, ...saltC]);
  ws.binary(Uint8Array.from([1, ...saltH]));
  const req = await nextMessage();
  assert.equal(req.t, "req");
  assert.equal(req.method, "POST");
  assert.equal(req.path, "/api/v1/pair");
  assert.equal(req.headers.authorization, undefined, "no token before pairing");
  assert.deepEqual(JSON.parse(Buffer.from(req.body, "base64").toString()), { token: "PAIRTOKEN" });
  await reply({
    t: "res",
    id: req.id,
    status: 200,
    headers: { "content-type": "application/json", "set-cookie": "harness_remote=DEVTOKEN; HttpOnly; Path=/" },
    body: Buffer.from('{"ok":true}').toString("base64"),
  });
  const res = await pending;
  assert.equal(res.status, 200);
  assert.deepEqual(await res.json(), { ok: true });
  assert.equal(JSON.parse(storage.get("worse-relay:abcdefghijklmnop")).token, "DEVTOKEN");
});

await step("later requests carry the bearer token; large replies are chunked", async () => {
  const pending = relay.relayFetch("/api/v1/status");
  const req = await nextMessage();
  assert.equal(req.headers.authorization, "Bearer DEVTOKEN");
  // A reply split into two chunks (flag bit 0 = more follows).
  const bytes = enc.encode(
    JSON.stringify({ t: "res", id: req.id, status: 200, headers: {}, body: Buffer.from('{"v":1}').toString("base64") }),
  );
  const cut = 10;
  ws.binary(await seal(h2c, sendSeq++, Uint8Array.from([1, ...bytes.subarray(0, cut)])));
  ws.binary(await seal(h2c, sendSeq++, Uint8Array.from([0, ...bytes.subarray(cut)])));
  assert.deepEqual(await (await pending).json(), { v: 1 });
});

await step("event stream opens on the first message and closes", async () => {
  const socket = relay.relaySocket("/api/v1/events");
  const seen = [];
  socket.addEventListener("open", () => seen.push("open"));
  socket.addEventListener("message", (e) => seen.push(e.data));
  socket.addEventListener("close", () => seen.push("close"));
  const req = await nextMessage();
  assert.equal(req.t, "ws-open");
  assert.equal(req.path, "/api/v1/events");
  assert.equal(req.headers.authorization, "Bearer DEVTOKEN");
  await reply({ t: "ws", id: req.id, data: '{"type":"hello"}' });
  await new Promise((r) => setTimeout(r, 10));
  assert.equal(socket.readyState, 1);
  await reply({ t: "ws-close", id: req.id });
  await new Promise((r) => setTimeout(r, 10));
  assert.deepEqual(seen, ["open", '{"type":"hello"}', "close"]);
});

await step("a forged frame drops the connection and fails pending calls", async () => {
  const pending = relay.relayFetch("/api/v1/status");
  await nextMessage();
  ws.binary(await seal(h2c, sendSeq + 5, Uint8Array.from([0, 1])));
  await assert.rejects(pending);
});

await step("desktop offline rejects right away", async () => {
  const pending = relay.relayFetch("/api/v1/status");
  await new Promise((r) => setTimeout(r, 0));
  const fresh = sockets.at(-1);
  assert.notEqual(fresh, ws, "a new socket after the drop");
  fresh.text({ t: "offline" });
  await assert.rejects(pending, /not running/);
});

console.log(failed ? `${failed} failed` : "all client checks passed");
process.exit(failed ? 1 : 0);
