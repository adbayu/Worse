#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const config = JSON.parse(fs.readFileSync(path.join(root, "scripts", "size-allow.json"), "utf8"));
const limit = config.limit;
const allow = new Map(Object.entries(config.allow).map(([file, entry]) => [file, entry.max]));
const roots = config.roots;
const skip = new Set(config.skipDirs);
const exts = new Set(config.extensions);

function* walk(dir) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (entry.isDirectory()) {
      if (!skip.has(entry.name)) yield* walk(path.join(dir, entry.name));
    } else if (exts.has(path.extname(entry.name))) {
      yield path.join(dir, entry.name);
    }
  }
}

const failures = [];
const stale = new Set(allow.keys());
let count = 0;
for (const base of roots) {
  for (const file of walk(path.join(root, base))) {
    count++;
    const rel = path.relative(root, file).split(path.sep).join("/");
    const text = fs.readFileSync(file, "utf8");
    const lines = text.length === 0 ? 0 : text.split("\n").length - (text.endsWith("\n") ? 1 : 0);
    const max = allow.get(rel) ?? limit;
    stale.delete(rel);
    if (lines > max) failures.push(`${rel}: ${lines} lines (max ${max})`);
  }
}
for (const rel of stale) failures.push(`${rel}: listed in size-allow.json but not found`);

if (failures.length) {
  console.error(`size check failed (${failures.length}):`);
  for (const line of failures) console.error(`  ${line}`);
  console.error(`Split the file by responsibility, or add a reasoned exception to scripts/size-allow.json.`);
  process.exit(1);
}
console.log(`size check ok: ${count} files, limit ${limit} lines, ${allow.size} exceptions`);
