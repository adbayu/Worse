// Worse Harness Remote Control relay (docs/plans/remote-relay.md). Routes:
//   GET /                      landing page
//   GET /healthz               {"ok":true}
//   GET /<id>                  phone web client (static assets in ./public)
//   GET /<id>/host  (ws)       desktop, signed with its Ed25519 key
//   GET /<id>/client (ws)      phone; payloads are end-to-end encrypted, the relay only forwards them

import { ID_RE, verifyHost } from "./auth";
import { landing } from "./pages";

export { Device } from "./device";

export interface Env {
  DEVICE: DurableObjectNamespace;
  ASSETS?: Fetcher;
}

const SECURITY_HEADERS = {
  "x-content-type-options": "nosniff",
  "referrer-policy": "no-referrer",
  "x-frame-options": "DENY",
};

function text(status: number, body: string): Response {
  return new Response(body, { status, headers: { "content-type": "text/plain; charset=utf-8", ...SECURITY_HEADERS } });
}

async function asset(request: Request, env: Env, path: string): Promise<Response> {
  if (!env.ASSETS) return text(404, "not found");
  const url = new URL(request.url);
  url.pathname = path;
  // Only method and URL reach the asset store; the client's cookies and headers do not.
  const response = await env.ASSETS.fetch(new Request(url, { method: request.method }));
  const headers = new Headers(response.headers);
  for (const [name, value] of Object.entries(SECURITY_HEADERS)) headers.set(name, value);
  return new Response(response.body, { status: response.status, headers });
}

/** Browsers send `Origin`; only pages of this relay may open a phone socket (native clients send none). */
function sameOrigin(request: Request, url: URL): boolean {
  const origin = request.headers.get("origin");
  return origin === null || origin === url.origin;
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const parts = url.pathname.split("/").filter(Boolean);
    if (request.method !== "GET" && request.method !== "HEAD") return text(405, "method not allowed");
    if (parts.length === 0) {
      return new Response(landing(), { headers: { "content-type": "text/html; charset=utf-8", ...SECURITY_HEADERS } });
    }
    if (parts.length === 1 && parts[0] === "healthz") {
      return Response.json({ ok: true });
    }
    if (parts[0] === "assets" || parts[0] === "sw.js" || parts[0] === "manifest.webmanifest") {
      return asset(request, env, url.pathname);
    }
    const id = parts[0] ?? "";
    if (!ID_RE.test(id)) return text(404, "not found");
    if (parts.length === 1) return asset(request, env, "/index.html");
    const role = parts[1];
    if (parts.length !== 2 || (role !== "host" && role !== "client")) return text(404, "not found");
    if (request.headers.get("upgrade")?.toLowerCase() !== "websocket") return text(426, "websocket required");
    if (role === "host") {
      const reason = await verifyHost(id, url.searchParams, Math.floor(Date.now() / 1000));
      if (reason) return text(403, reason);
    } else if (!sameOrigin(request, url)) {
      return text(403, "origin not allowed");
    }
    const stub = env.DEVICE.get(env.DEVICE.idFromName(id));
    const forward = new URL(request.url);
    forward.search = "";
    forward.pathname = `/${role}`;
    return stub.fetch(new Request(forward, request));
  },
} satisfies ExportedHandler<Env>;
