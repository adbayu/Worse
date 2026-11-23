#!/usr/bin/env node
// Relay tests against a local `wrangler dev` (no Cloudflare account, nothing deployed).
// Run: npm test   (uses port 8799 on 127.0.0.1)
import { spawn, execSync } from "node:child_process";
import crypto from "node:crypto";
import assert from "node:assert/strict";

const PORT = 8799;
const BASE = `http://127.0.0.1:${PORT}`;
const WS = `ws://127.0.0.1:${PORT}`;
const B32 = "abcdefghijklmnopqrstuvwxyz234567";

function base32(bytes) {
  let out = "";
  let bits = 0;
  let value = 0;
  for (const byte of bytes) {
    value = (value << 8) | byte;
    bits += 8;
    while (bits >= 5) {
      out += B32[(value >>> (bits - 5)) & 31];
      bits -= 5;
    }
  }
  return bits > 0 ? out + B32[(value << (5 - bits)) & 31] : out;
}

function identity() {
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ed25519");
  const pub = Buffer.from(publicKey.export({ format: "jwk" }).x, "base64url");
  const id = base32(crypto.createHash("sha256").update(pub).digest()).slice(0, 16);
  const hostUrl = (forId = id, ts = Math.floor(Date.now() / 1000)) => {
    const sig = crypto.sign(null, Buffer.from(`worse-relay/1 host ${forId} ${ts}`), privateKey);
    return `${WS}/${forId}/host?pub=${pub.toString("base64url")}&ts=${ts}&sig=${sig.toString("base64url")}`;
  };
  return { id, hostUrl };
}

/** A socket with an awaitable message queue. */
function open(url) {
  const ws = new WebSocket(url);
  ws.binaryType = "arraybuffer";
  const queue = [];
  const waiters = [];
  const push = (item) => (waiters.length ? waiters.shift()(item) : queue.push(item));
  ws.addEventListener("message", (e) => push(typeof e.data === "string" ? e.data : new Uint8Array(e.data)));
  ws.addEventListener("close", (e) => push({ closed: e.code }));
  const ready = new Promise((resolve, reject) => {
    ws.addEventListener("open", () => resolve(ws));
    ws.addEventListener("error", () => reject(new Error(`cannot open ${url.split("?")[0]}`)));
  });
  const next = (ms = 5000) =>
    queue.length
      ? Promise.resolve(queue.shift())
      : new Promise((resolve, reject) => {
          const waiter = (item) => {
            clearTimeout(timer);
            resolve(item);
          };
          const timer = setTimeout(() => {
            waiters.splice(waiters.indexOf(waiter), 1);
            reject(new Error("timeout waiting for a message"));
          }, ms);
          waiters.push(waiter);
        });
  return { ws, ready, next };
}

const json = (item) => JSON.parse(item);
const KICKED = JSON.stringify({ t: "error", m: "closed by the desktop" });

async function waitForServer(child) {
  for (let i = 0; i < 120; i++) {
    if (child.exitCode !== null) throw new Error("wrangler dev exited");
    try {
      if ((await fetch(`${BASE}/healthz`)).ok) return;
    } catch {}
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error("wrangler dev did not start");
}

const tests = [];
const test = (name, fn) => tests.push({ name, fn });

test("pages and ids", async () => {
  assert.equal((await fetch(`${BASE}/`)).status, 200);
  assert.deepEqual(await (await fetch(`${BASE}/healthz`)).json(), { ok: true });
  assert.equal((await fetch(`${BASE}/not-an-id`)).status, 404);
  assert.equal((await fetch(`${BASE}/abcdefghijklmnop/client`)).status, 426);
});

test("host needs a valid signature for its own id", async () => {
  const me = identity();
  const other = identity();
  await assert.rejects(open(me.hostUrl(other.id)).ready);
  await assert.rejects(open(me.hostUrl(me.id, Math.floor(Date.now() / 1000) - 3600)).ready);
  await assert.rejects(open(`${WS}/${me.id}/host?pub=AAAA&ts=1&sig=AAAA`).ready);
});

test("frames flow between phone and desktop", async () => {
  const me = identity();
  const phone = open(`${WS}/${me.id}/client`);
  await phone.ready;
  assert.deepEqual(json(await phone.next()), { t: "offline" });
  const host = open(me.hostUrl());
  await host.ready;
  const opened = json(await host.next());
  assert.equal(opened.t, "open");
  assert.deepEqual(json(await phone.next()), { t: "online" });

  phone.ws.send(new Uint8Array([1, 2, 3]));
  const up = await host.next();
  assert.deepEqual([...up.slice(4)], [1, 2, 3]);
  assert.equal(new DataView(up.buffer).getUint32(0), opened.c);

  const down = new Uint8Array([0, 0, 0, 0, 9, 9]);
  new DataView(down.buffer).setUint32(0, opened.c);
  host.ws.send(down);
  assert.deepEqual([...(await phone.next())], [9, 9]);

  phone.ws.send("ping");
  assert.equal(await phone.next(), "pong");

  phone.ws.close(1000);
  assert.deepEqual(json(await host.next()), { t: "close", c: opened.c });
  host.ws.close(1000);
});

test("desktop kicks a phone, phones see the desktop go offline", async () => {
  const me = identity();
  const host = open(me.hostUrl());
  await host.ready;
  const a = open(`${WS}/${me.id}/client`);
  const b = open(`${WS}/${me.id}/client`);
  await Promise.all([a.ready, b.ready]);
  assert.deepEqual(json(await a.next()), { t: "online" });
  assert.deepEqual(json(await b.next()), { t: "online" });
  const first = json(await host.next());
  const second = json(await host.next());
  const kicked = first.c;
  const target = [a, b];
  host.ws.send(JSON.stringify({ t: "kick", c: kicked }));
  const results = await Promise.all(target.map((p) => p.next(1500).catch(() => null)));
  assert.ok(results.some((r) => r === KICKED), `one phone is closed with 4001: ${JSON.stringify(results)}`);
  const survivor = target[results.findIndex((r) => r !== KICKED)];
  assert.deepEqual(json(await host.next()), { t: "close", c: kicked });
  const otherCid = [first.c, second.c].find((c) => c !== kicked);
  const frame = new Uint8Array([0, 0, 0, 0, 7]);
  new DataView(frame.buffer).setUint32(0, otherCid);
  host.ws.send(frame);
  assert.deepEqual([...(await survivor.next())], [7]);
  host.ws.close(1000);
  assert.deepEqual(json(await survivor.next()), { t: "offline" });
  survivor.ws.close(1000);
});

test("phones see the desktop go offline", async () => {
  const me = identity();
  const host = open(me.hostUrl());
  await host.ready;
  const phone = open(`${WS}/${me.id}/client`);
  await phone.ready;
  assert.deepEqual(json(await phone.next()), { t: "online" });
  host.ws.close(1000);
  assert.deepEqual(json(await phone.next()), { t: "offline" });
  phone.ws.close(1000);
});

test("a new desktop replaces the old one", async () => {
  const me = identity();
  const first = open(me.hostUrl());
  await first.ready;
  const phone = open(`${WS}/${me.id}/client`);
  await phone.ready;
  assert.deepEqual(json(await phone.next()), { t: "online" });
  assert.equal(json(await first.next()).t, "open");
  const second = open(me.hostUrl());
  await second.ready;
  assert.deepEqual(json(await first.next()), { t: "replaced" });
  // The phone was talking to the old desktop: it is closed and must reconnect.
  assert.deepEqual(json(await phone.next()), { t: "error", m: "desktop reconnected" });
  second.ws.close(1000);
});

const child = spawn(`npx wrangler dev --port ${PORT} --ip 127.0.0.1`, {
  shell: true,
  stdio: ["ignore", "pipe", "pipe"],
});
let log = "";
child.stdout.on("data", (d) => (log += d));
child.stderr.on("data", (d) => (log += d));
let failed = 0;
try {
  execSync("node scripts/sync-assets.mjs", { stdio: "ignore" });
  await waitForServer(child);
  for (const { name, fn } of tests) {
    try {
      await fn();
      console.log(`ok   ${name}`);
    } catch (error) {
      failed++;
      console.log(`FAIL ${name}\n     ${error.stack ?? error}`);
    }
  }
} catch (error) {
  failed++;
  console.log(`FAIL setup: ${error.message}\n${log.slice(-2000)}`);
} finally {
  if (process.platform === "win32") {
    try {
      execSync(`taskkill /pid ${child.pid} /t /f`, { stdio: "ignore" });
    } catch {}
  } else {
    child.kill("SIGTERM");
  }
}
console.log(failed ? `${failed} failed` : `all ${tests.length} passed`);
process.exit(failed ? 1 : 0);
