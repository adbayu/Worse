// One Durable Object per desktop: joins the desktop's socket ("host") with up to MAX_CLIENTS phone sockets and
// forwards opaque (end-to-end encrypted) frames between them. Uses the WebSocket Hibernation API, so idle
// connections cost no duration; nothing is written to storage.

import { DurableObject } from "cloudflare:workers";
import type { Env } from "./index";

const MAX_CLIENTS = 8;
const MAX_FRAME = 1024 * 1024;
const RATE_WINDOW_MS = 60_000;
const RATE_MAX = 1200;
const OPEN = 1;
const CLOSED = 3;

type Attachment = { role: "host" } | { role: "client"; cid: number };

function attachment(ws: WebSocket): Attachment | null {
  const value = ws.deserializeAttachment() as Attachment | null;
  return value && (value.role === "host" || value.role === "client") ? value : null;
}

function safeSend(ws: WebSocket, data: string | ArrayBuffer | Uint8Array): void {
  try {
    ws.send(data);
  } catch {
    // The peer is gone; its close handler cleans up.
  }
}

function safeClose(ws: WebSocket, code: number, reason: string): void {
  try {
    ws.close(code, reason);
  } catch {
    // Already closed.
  }
}

export class Device extends DurableObject<Env> {
  private nextCid = 0;
  private rates = new Map<WebSocket, { start: number; count: number }>();

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    ctx.setWebSocketAutoResponse(new WebSocketRequestResponsePair("ping", "pong"));
  }

  private host(except?: WebSocket): WebSocket | undefined {
    return this.ctx.getWebSockets("host").find((ws) => ws !== except && ws.readyState === OPEN);
  }

  private clients(): WebSocket[] {
    return this.ctx.getWebSockets("client").filter((ws) => ws.readyState === OPEN);
  }

  private allocateCid(): number {
    if (this.nextCid === 0) {
      for (const ws of this.ctx.getWebSockets("client")) {
        const att = attachment(ws);
        if (att?.role === "client") this.nextCid = Math.max(this.nextCid, att.cid);
      }
    }
    this.nextCid = (this.nextCid % 0xffff_fffe) + 1;
    return this.nextCid;
  }

  override async fetch(request: Request): Promise<Response> {
    const role = new URL(request.url).pathname === "/host" ? "host" : "client";
    if (role === "client" && this.clients().length >= MAX_CLIENTS) {
      return new Response("too many phones connected", { status: 429 });
    }
    const pair = new WebSocketPair();
    const [client, server] = [pair[0], pair[1]];
    if (role === "host") {
      const replaced = this.ctx.getWebSockets("host").length > 0;
      for (const old of this.ctx.getWebSockets("host")) {
        safeSend(old, JSON.stringify({ t: "replaced" }));
        safeClose(old, 4000, "replaced by a new host");
      }
      this.ctx.acceptWebSocket(server, ["host"]);
      server.serializeAttachment({ role: "host" } satisfies Attachment);
      for (const phone of this.clients()) {
        // Phones mid-session with the old host hold its keys: close them so they reconnect and handshake
        // again. Phones that were waiting while the desktop was offline get "online" and start their handshake.
        if (replaced) {
          safeSend(phone, JSON.stringify({ t: "error", m: "desktop reconnected" }));
          safeClose(phone, 4002, "desktop reconnected");
          continue;
        }
        const att = attachment(phone);
        if (att?.role === "client") safeSend(server, JSON.stringify({ t: "open", c: att.cid }));
        safeSend(phone, JSON.stringify({ t: "online" }));
      }
    } else {
      const cid = this.allocateCid();
      this.ctx.acceptWebSocket(server, ["client", `c${cid}`]);
      server.serializeAttachment({ role: "client", cid } satisfies Attachment);
      const host = this.host();
      if (host) safeSend(host, JSON.stringify({ t: "open", c: cid }));
      safeSend(server, JSON.stringify({ t: host ? "online" : "offline" }));
    }
    return new Response(null, { status: 101, webSocket: client });
  }

  private limited(ws: WebSocket, size: number): boolean {
    if (size > MAX_FRAME) {
      safeClose(ws, 1009, "frame too large");
      return true;
    }
    const now = Date.now();
    const rate = this.rates.get(ws);
    if (!rate || now - rate.start > RATE_WINDOW_MS) {
      this.rates.set(ws, { start: now, count: 1 });
      return false;
    }
    rate.count += 1;
    if (rate.count > RATE_MAX) {
      safeClose(ws, 1008, "too many messages");
      return true;
    }
    return false;
  }

  override async webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    const att = attachment(ws);
    const size = typeof message === "string" ? message.length : message.byteLength;
    if (!att || this.limited(ws, size)) return;
    if (att.role === "client") {
      if (typeof message === "string") return;
      const host = this.host();
      if (!host) {
        safeSend(ws, JSON.stringify({ t: "offline" }));
        return;
      }
      const frame = new Uint8Array(4 + message.byteLength);
      new DataView(frame.buffer).setUint32(0, att.cid);
      frame.set(new Uint8Array(message), 4);
      safeSend(host, frame);
      return;
    }
    if (typeof message === "string") {
      let control: { t?: unknown; c?: unknown };
      try {
        control = JSON.parse(message) as { t?: unknown; c?: unknown };
      } catch {
        return;
      }
      if (control.t === "kick" && typeof control.c === "number") {
        for (const phone of this.ctx.getWebSockets(`c${control.c}`)) {
          safeSend(phone, JSON.stringify({ t: "error", m: "closed by the desktop" }));
          safeClose(phone, 4001, "closed by the desktop");
        }
      }
      return;
    }
    if (message.byteLength < 4) return;
    const cid = new DataView(message).getUint32(0);
    const phone = this.ctx.getWebSockets(`c${cid}`).find((p) => p.readyState === OPEN);
    if (phone) safeSend(phone, message.slice(4));
    else safeSend(ws, JSON.stringify({ t: "close", c: cid }));
  }

  private gone(ws: WebSocket): void {
    this.rates.delete(ws);
    const att = attachment(ws);
    if (att?.role === "client") {
      const host = this.host();
      if (host) safeSend(host, JSON.stringify({ t: "close", c: att.cid }));
    } else if (att?.role === "host" && !this.host(ws)) {
      for (const phone of this.clients()) safeSend(phone, JSON.stringify({ t: "offline" }));
    }
  }

  override async webSocketClose(ws: WebSocket, code: number): Promise<void> {
    this.gone(ws);
    // Answer a close the peer started; sockets the relay closed itself (kick, replace) are already CLOSED.
    if (ws.readyState !== CLOSED) safeClose(ws, code === 1005 || code === 1006 ? 1000 : code, "closed");
  }

  override async webSocketError(ws: WebSocket): Promise<void> {
    this.gone(ws);
  }
}
