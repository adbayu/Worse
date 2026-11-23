/*
 * Relay transport for the phone client (docs/plans/remote-relay.md §3–4).
 *
 * When the page is opened from the relay (`https://<relay>/<device-id>#k=…`), every API call and the event
 * stream travel through one WebSocket to the relay, end-to-end encrypted with the link secret `k` (WebCrypto
 * HKDF-SHA256 + AES-256-GCM). The relay only sees ciphertext. Elsewhere (desktop loopback / tunnel) this
 * module stays inactive and app.js uses plain fetch/WebSocket.
 *
 * Exposes a fetch-like `relayFetch` (returns a real Response) and `relaySocket` (a WebSocket-like object).
 * The device token comes back from `POST /api/v1/pair` in Set-Cookie; this module keeps it in localStorage
 * like a cookie jar and sends it as `Authorization: Bearer`.
 */

const ID_RE = /^\/([a-z2-7]{16})\/?$/;
const HELLO = 1;
const DATA = 2;
const MORE = 1;
const CHUNK = 256 * 1024;
const MAX_MESSAGE = 32 * 1024 * 1024;
const MAX_RELAY_BODY = 22 * 1024 * 1024;
const REQUEST_TIMEOUT_MS = 60000;
const READY_TIMEOUT_MS = 20000;
const BACKOFF_MIN = 1000;
const BACKOFF_MAX = 30000;
const COOKIE = 'harness_remote=';

const match = ID_RE.exec(location.pathname);
const deviceId = match ? match[1] : '';
const storeKey = 'worse-relay:' + deviceId;
const enc = new TextEncoder();
const dec = new TextDecoder();

function loadStore() {
  try {
    return JSON.parse(localStorage.getItem(storeKey) || '{}') || {};
  } catch (err) {
    return {};
  }
}

function saveStore(value) {
  try {
    localStorage.setItem(storeKey, JSON.stringify(value));
  } catch (err) {
    /* private mode: the link keeps working for this tab only */
  }
}

let store = deviceId ? loadStore() : {};
/** Pair token from the QR (`&p=`), redeemed once by app.js at boot. */
let pairToken = '';

if (deviceId) {
  // The fragment never reaches the relay. Keep `k`, hand `p` to app.js, then clear it from the address bar.
  const fragment = new URLSearchParams(location.hash.replace(/^#/, ''));
  const k = fragment.get('k');
  if (k) {
    if (k !== store.k) store = { k };
    pairToken = fragment.get('p') || '';
    saveStore(store);
    history.replaceState(null, '', location.pathname);
  }
}

export const relayActive = Boolean(deviceId);

/** True when the page came from the relay but this phone has never seen the link secret. */
export function relayMissingSecret() {
  return relayActive && !store.k;
}

export function takePairToken() {
  const token = pairToken;
  pairToken = '';
  return token;
}

/** Drops the device token (after a 401) so the pair screen shows. */
export function relayForget() {
  if (!relayActive) return;
  delete store.token;
  saveStore(store);
}

/* ---------- bytes ---------- */

function b64urlDecode(text) {
  const b64 = text.replace(/-/g, '+').replace(/_/g, '/');
  const bin = atob(b64 + '==='.slice((b64.length + 3) % 4));
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function b64Encode(bytes) {
  let bin = '';
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

function b64Decode(text) {
  const bin = atob(text || '');
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function header(seq) {
  const head = new Uint8Array(9);
  head[0] = DATA;
  new DataView(head.buffer).setBigUint64(1, BigInt(seq));
  return head;
}

function nonce(seq) {
  const iv = new Uint8Array(12);
  new DataView(iv.buffer).setBigUint64(4, BigInt(seq));
  return iv;
}

async function deriveKey(secret, salt, info) {
  const base = await crypto.subtle.importKey('raw', secret, 'HKDF', false, ['deriveKey']);
  return crypto.subtle.deriveKey(
    { name: 'HKDF', hash: 'SHA-256', salt, info: enc.encode(info) },
    base,
    { name: 'AES-GCM', length: 256 },
    false,
    ['encrypt', 'decrypt'],
  );
}

/* ---------- connection ---------- */

const conn = {
  ws: null,
  /** 'idle' | 'connecting' | 'offline' | 'hello' | 'ready' */
  phase: 'idle',
  salt: null,
  send: null,
  recv: null,
  sendSeq: 0,
  recvSeq: 0,
  chunks: [],
  chunkBytes: 0,
  /** Serializes decryption so frames are opened in order. */
  inbox: Promise.resolve(),
  /** Serializes encryption so sequence numbers go out in order. */
  outbox: Promise.resolve(),
  waiters: [],
  backoff: BACKOFF_MIN,
  retry: 0,
  nextId: 1,
  pending: new Map(),
  streams: new Map(),
};

function wsUrl() {
  return (location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/' + deviceId + '/client';
}

function open() {
  if (conn.ws || !store.k) return;
  clearTimeout(conn.retry);
  conn.phase = 'connecting';
  let ws;
  try {
    ws = new WebSocket(wsUrl());
  } catch (err) {
    scheduleReconnect();
    return;
  }
  ws.binaryType = 'arraybuffer';
  conn.ws = ws;
  ws.addEventListener('message', (event) => {
    if (conn.ws !== ws) return;
    if (typeof event.data === 'string') onControl(event.data);
    else onBinary(new Uint8Array(event.data));
  });
  ws.addEventListener('close', () => {
    if (conn.ws !== ws) return;
    conn.ws = null;
    reset('connection to the relay closed');
    scheduleReconnect();
  });
  ws.addEventListener('error', () => {
    try {
      ws.close();
    } catch (err) {
      /* closed */
    }
  });
}

function scheduleReconnect() {
  clearTimeout(conn.retry);
  const delay = conn.backoff;
  conn.backoff = Math.min(BACKOFF_MAX, conn.backoff * 2);
  conn.retry = setTimeout(() => {
    // Reconnect only while something needs the desktop; the next call reopens it otherwise.
    if (conn.pending.size || conn.streams.size || conn.waiters.length) open();
  }, delay);
}

/** Ends the current E2E session: fails in-flight requests, closes streams. */
function reset(reason) {
  conn.phase = conn.ws ? 'offline' : 'idle';
  conn.send = conn.recv = conn.salt = null;
  conn.sendSeq = conn.recvSeq = 0;
  conn.chunks = [];
  conn.chunkBytes = 0;
  for (const { reject, timer } of conn.pending.values()) {
    clearTimeout(timer);
    reject(new Error(reason));
  }
  conn.pending.clear();
  for (const stream of conn.streams.values()) stream._closed();
  conn.streams.clear();
}

function onControl(text) {
  let control;
  try {
    control = JSON.parse(text);
  } catch (err) {
    return;
  }
  if (!control) return;
  if (control.t === 'online') startHello();
  // The relay is about to close this socket (kicked, desktop reconnected): close it now and start over.
  else if (control.t === 'error') dropConnection();
  else if (control.t === 'offline') {
    reset('Harness is not running on the desktop');
    flushWaiters(new Error('Harness is not running on the desktop'));
  }
}

function startHello() {
  conn.backoff = BACKOFF_MIN;
  conn.phase = 'hello';
  conn.salt = crypto.getRandomValues(new Uint8Array(16));
  const hello = new Uint8Array(17);
  hello[0] = HELLO;
  hello.set(conn.salt, 1);
  conn.ws.send(hello);
}

function onBinary(data) {
  if (conn.phase === 'hello') {
    if (data.length !== 17 || data[0] !== HELLO) return;
    const salt = new Uint8Array(32);
    salt.set(conn.salt, 0);
    salt.set(data.subarray(1), 16);
    const secret = b64urlDecode(store.k);
    conn.phase = 'keys';
    Promise.all([deriveKey(secret, salt, 'worse-relay/1 c2h'), deriveKey(secret, salt, 'worse-relay/1 h2c')])
      .then(([send, recv]) => {
        conn.send = send;
        conn.recv = recv;
        conn.phase = 'ready';
        flushWaiters(null);
      })
      .catch(() => conn.ws && conn.ws.close());
    return;
  }
  if (conn.phase !== 'ready' && conn.phase !== 'keys') return;
  const ws = conn.ws;
  conn.inbox = conn.inbox.then(() => (conn.ws === ws ? openFrame(data) : undefined));
}

async function openFrame(frame) {
  if (frame.length < 25 || frame[0] !== DATA || !conn.recv) return dropConnection();
  const seq = Number(new DataView(frame.buffer, frame.byteOffset).getBigUint64(1));
  if (seq !== conn.recvSeq) return dropConnection();
  let plain;
  try {
    plain = new Uint8Array(
      await crypto.subtle.decrypt(
        { name: 'AES-GCM', iv: nonce(seq), additionalData: header(seq) },
        conn.recv,
        frame.subarray(9),
      ),
    );
  } catch (err) {
    return dropConnection();
  }
  conn.recvSeq += 1;
  if (!plain.length) return dropConnection();
  conn.chunks.push(plain.subarray(1));
  conn.chunkBytes += plain.length - 1;
  if (conn.chunkBytes > MAX_MESSAGE) return dropConnection();
  if (plain[0] & MORE) return undefined;
  const whole = new Uint8Array(conn.chunkBytes);
  let at = 0;
  for (const part of conn.chunks) {
    whole.set(part, at);
    at += part.length;
  }
  conn.chunks = [];
  conn.chunkBytes = 0;
  let message;
  try {
    message = JSON.parse(dec.decode(whole));
  } catch (err) {
    return undefined;
  }
  onMessage(message);
  return undefined;
}

function dropConnection() {
  if (conn.ws) conn.ws.close();
}

function onMessage(message) {
  if (!message || typeof message !== 'object') return;
  if (message.t === 'res') {
    const entry = conn.pending.get(message.id);
    if (!entry) return;
    conn.pending.delete(message.id);
    clearTimeout(entry.timer);
    entry.resolve(message);
  } else if (message.t === 'ws') {
    const stream = conn.streams.get(message.id);
    if (stream) stream._message(String(message.data || ''));
  } else if (message.t === 'ws-close') {
    const stream = conn.streams.get(message.id);
    conn.streams.delete(message.id);
    if (stream) stream._closed();
  }
}

function flushWaiters(error) {
  const waiters = conn.waiters;
  conn.waiters = [];
  for (const { resolve, reject, timer } of waiters) {
    clearTimeout(timer);
    if (error) reject(error);
    else resolve();
  }
}

function ready() {
  if (conn.phase === 'ready') return Promise.resolve();
  if (!store.k) return Promise.reject(new Error('Scan the QR code on the desktop to connect.'));
  open();
  return new Promise((resolve, reject) => {
    const waiter = { resolve, reject, timer: 0 };
    waiter.timer = setTimeout(() => {
      conn.waiters = conn.waiters.filter((w) => w !== waiter);
      reject(new Error('Cannot reach the desktop through the relay.'));
    }, READY_TIMEOUT_MS);
    conn.waiters.push(waiter);
  });
}

/** Encrypts and sends one inner message (chunked); calls stay in order. */
function post(message) {
  const ws = conn.ws;
  const bytes = enc.encode(JSON.stringify(message));
  const job = conn.outbox.then(async () => {
    if (conn.ws !== ws || conn.phase !== 'ready') throw new Error('relay connection lost');
    const count = Math.max(1, Math.ceil(bytes.length / CHUNK));
    for (let i = 0; i < count; i++) {
      const part = bytes.subarray(i * CHUNK, (i + 1) * CHUNK);
      const plain = new Uint8Array(part.length + 1);
      plain[0] = i + 1 < count ? MORE : 0;
      plain.set(part, 1);
      const seq = conn.sendSeq;
      conn.sendSeq += 1;
      const head = header(seq);
      const sealed = new Uint8Array(
        await crypto.subtle.encrypt({ name: 'AES-GCM', iv: nonce(seq), additionalData: head }, conn.send, plain),
      );
      const frame = new Uint8Array(9 + sealed.length);
      frame.set(head, 0);
      frame.set(sealed, 9);
      ws.send(frame);
    }
  });
  conn.outbox = job.catch(() => undefined);
  return job;
}

function authHeaders(extra) {
  const headers = { ...(extra || {}) };
  if (store.token) headers.authorization = 'Bearer ' + store.token;
  return headers;
}

function keepCookie(setCookie) {
  if (!setCookie) return;
  const first = String(setCookie).split(';')[0].trim();
  if (!first.startsWith(COOKIE)) return;
  store.token = first.slice(COOKIE.length);
  saveStore(store);
}

async function bodyBytes(body) {
  if (body == null) return new Uint8Array(0);
  if (typeof body === 'string') return enc.encode(body);
  if (body instanceof Uint8Array) return body;
  if (body instanceof ArrayBuffer) return new Uint8Array(body);
  if (body instanceof Blob) return new Uint8Array(await body.arrayBuffer());
  return enc.encode(String(body));
}

/** `fetch` over the relay for `/api/v1/*` and `/healthz`; resolves to a real `Response`. */
export async function relayFetch(path, options) {
  const opts = options || {};
  const body = await bodyBytes(opts.body);
  // Base64 grows the body by a third; the desktop joins at most 32 MiB per message.
  if (body.length > MAX_RELAY_BODY) {
    return new Response(null, { status: 413 });
  }
  await ready();
  const id = conn.nextId++;
  const reply = new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      conn.pending.delete(id);
      reject(new Error('the desktop did not answer'));
    }, REQUEST_TIMEOUT_MS);
    conn.pending.set(id, { resolve, reject, timer });
  });
  await post({
    t: 'req',
    id,
    method: opts.method || 'GET',
    path,
    headers: authHeaders(opts.headers),
    body: body.length ? b64Encode(body) : '',
  });
  const res = await reply;
  const headers = res.headers || {};
  keepCookie(headers['set-cookie']);
  const status = res.status >= 200 && res.status <= 599 ? res.status : 502;
  const noBody = status === 204 || status === 205 || status === 304;
  return new Response(noBody ? null : b64Decode(res.body), {
    status,
    headers: headers['content-type'] ? { 'content-type': headers['content-type'] } : {},
  });
}

/** A WebSocket-like stream (`/api/v1/events`) over the relay. */
class RelaySocket extends EventTarget {
  constructor(path) {
    super();
    this.readyState = 0;
    this.id = 0;
    this.path = path;
    this._start();
  }

  async _start() {
    try {
      await ready();
      if (this.readyState !== 0) return;
      this.id = conn.nextId++;
      conn.streams.set(this.id, this);
      await post({ t: 'ws-open', id: this.id, path: this.path, headers: authHeaders() });
    } catch (err) {
      this._closed();
    }
  }

  _message(data) {
    if (this.readyState === 0) {
      // The server greets right away, so the first message also means "open".
      this.readyState = 1;
      this.dispatchEvent(new Event('open'));
    }
    if (this.readyState === 1) this.dispatchEvent(new MessageEvent('message', { data }));
  }

  _closed() {
    if (this.readyState === 3) return;
    this.readyState = 3;
    this.dispatchEvent(new Event('close'));
  }

  send(data) {
    if (this.readyState === 1) post({ t: 'ws', id: this.id, data: String(data) }).catch(() => undefined);
  }

  close() {
    if (this.readyState === 3) return;
    if (this.id && conn.streams.get(this.id) === this) {
      conn.streams.delete(this.id);
      post({ t: 'ws-close', id: this.id }).catch(() => undefined);
    }
    this._closed();
  }
}

export function relaySocket(path) {
  return new RelaySocket(path);
}
