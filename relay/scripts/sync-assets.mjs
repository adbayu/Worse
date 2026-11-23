#!/usr/bin/env node
// Copies the phone web client (crates/harness-remote/src/assets) into relay/public for `wrangler dev`/`deploy`.
// The same files are served by the desktop's loopback server, so there is one client for both transports.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const relay = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const source = path.resolve(relay, "..", "crates", "harness-remote", "src", "assets");
const target = path.join(relay, "public");

fs.rmSync(target, { recursive: true, force: true });
fs.mkdirSync(path.join(target, "assets"), { recursive: true });
let count = 0;
for (const name of fs.readdirSync(source)) {
  const from = path.join(source, name);
  if (!fs.statSync(from).isFile()) continue;
  // Like the desktop server: every file under /assets/, index.html and sw.js also at the root.
  fs.copyFileSync(from, path.join(target, "assets", name));
  if (name === "index.html" || name === "sw.js") fs.copyFileSync(from, path.join(target, name));
  count++;
}
console.log(`synced ${count} client files into ${path.relative(process.cwd(), target) || "."}`);
