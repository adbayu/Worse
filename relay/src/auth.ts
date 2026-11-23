// Desktop (host) identity: device-id = base32(SHA-256(Ed25519 public key))[0..16], and a signed timestamp proves
// the connecting host owns that key (docs/plans/remote-relay.md §1–2).

export const ID_LEN = 16;
export const ID_RE = /^[a-z2-7]{16}$/;
const MAX_SKEW_S = 300;
const B32 = "abcdefghijklmnopqrstuvwxyz234567";

export function base32(bytes: Uint8Array): string {
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
  if (bits > 0) out += B32[(value << (5 - bits)) & 31];
  return out;
}

export function fromBase64Url(text: string): Uint8Array | null {
  if (!/^[A-Za-z0-9_-]*$/.test(text)) return null;
  const b64 = text.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((text.length + 3) % 4);
  try {
    return Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
  } catch {
    return null;
  }
}

export async function deviceId(publicKey: Uint8Array): Promise<string> {
  const hash = new Uint8Array(await crypto.subtle.digest("SHA-256", publicKey));
  return base32(hash).slice(0, ID_LEN);
}

export function hostMessage(id: string, ts: number): Uint8Array {
  return new TextEncoder().encode(`worse-relay/1 host ${id} ${ts}`);
}

/** `null` when the host may connect, else the reason. */
export async function verifyHost(id: string, params: URLSearchParams, nowS: number): Promise<string | null> {
  const pub = fromBase64Url(params.get("pub") ?? "");
  const sig = fromBase64Url(params.get("sig") ?? "");
  const ts = Number(params.get("ts"));
  if (!pub || pub.length !== 32 || !sig || sig.length !== 64) return "bad key or signature";
  if (!Number.isSafeInteger(ts) || Math.abs(nowS - ts) > MAX_SKEW_S) return "clock skew";
  if ((await deviceId(pub)) !== id) return "key does not match the device id";
  try {
    const key = await crypto.subtle.importKey("raw", pub, { name: "Ed25519" }, false, ["verify"]);
    const ok = await crypto.subtle.verify({ name: "Ed25519" }, key, sig, hostMessage(id, ts));
    return ok ? null : "bad signature";
  } catch {
    return "bad signature";
  }
}
