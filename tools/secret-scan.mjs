#!/usr/bin/env node
/**
 * T-059 secret scan over the built browser bundle.
 *
 * Two rules:
 *   1. Named secrets that must never be imported into `apps/web`.
 *   2. A 40+ character base64-looking blob, the shortest shape a leaked key
 *      usually takes. Source maps are not emitted, so any match is suspicious.
 *
 * Usage: node tools/secret-scan.mjs [dist-dir]
 * Exit 0 clean, 1 findings, 2 missing directory.
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

const dir = process.argv[2] ?? "apps/web/dist";
const named = [
  "SESSION_SIGNING_KEY",
  "ROOM_REGISTRY_TOKEN",
  "CLOUDFLARE_API_TOKEN",
  "CF_API_TOKEN",
  "PRIVATE_KEY",
  "BEGIN RSA PRIVATE KEY",
  "BEGIN OPENSSH PRIVATE KEY",
];
const base64 = /[A-Za-z0-9+/]{40,}={0,2}/g;
const textExtensions = new Set([".js", ".css", ".html", ".json", ".svg", ".txt"]);

function walk(path) {
  const out = [];
  for (const entry of readdirSync(path)) {
    const full = join(path, entry);
    if (statSync(full).isDirectory()) out.push(...walk(full));
    else out.push(full);
  }
  return out;
}

let files;
try {
  files = walk(dir);
} catch (error) {
  console.error(`secret-scan: cannot read ${dir}: ${error.message}`);
  process.exit(2);
}

const findings = [];
for (const file of files) {
  const extension = file.slice(file.lastIndexOf("."));
  if (!textExtensions.has(extension)) continue;
  const text = readFileSync(file, "utf8");
  for (const secret of named) {
    if (text.includes(secret)) findings.push(`${file}: named secret ${secret}`);
  }
  for (const match of text.matchAll(base64)) {
    findings.push(`${file}: base64-looking blob (${match[0].length} chars)`);
  }
}

if (findings.length > 0) {
  for (const finding of findings) console.error(`secret-scan: ${finding}`);
  process.exit(1);
}
console.log(`secret-scan: ${files.length} files clean`);
