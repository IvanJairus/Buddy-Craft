#!/usr/bin/env node
/**
 * T-058 protocol documentation generator.
 *
 * Reads the Rust source, not a hand-maintained copy, so a tag or field name
 * cannot drift from `docs/PROTOCOL.md`. Run from the repository root:
 *   node tools/protocol-doc.mjs
 */
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";

const source = readFileSync("crates/protocol/src/lib.rs", "utf8");
const digest = createHash("sha256").update(source).digest("hex").slice(0, 12);

function enumRows(name) {
  const lines = source.split("\n");
  const start = lines.findIndex((line) => line.includes(`${name} {`));
  if (start < 0) throw new Error(`wire enum ${name} not found`);
  const rows = [];
  for (let i = start + 1; i < lines.length; i += 1) {
    const line = lines[i];
    if (/^\s*}\s*$/.test(line)) break;
    const match = /^\s*(\w+)\s*=\s*(0x[0-9a-fA-F]+)\s*=>\s*(\w+)/.exec(line);
    if (match) rows.push({ name: match[1], tag: match[2], payload: match[3] });
  }
  if (rows.length === 0) throw new Error(`wire enum ${name} had no variants`);
  return rows;
}

function table(rows) {
  return [
    "| Variant | Tag | Payload type |",
    "| --- | --- | --- |",
    ...rows.map((row) => `| ${row.name} | \`${row.tag}\` | \`${row.payload}\` |`),
  ].join("\n");
}

const c2s = enumRows("C2s");
const s2c = enumRows("S2c");
const constants = [...source.matchAll(/pub const (PROTOCOL_VERSION|MAX_\w+):[^=]+=\s*([^;]+);/g)]
  .map((match) => `- \`${match[1]}\`: \`${match[2].trim()}\``)
  .join("\n");
const structs = [...source.matchAll(/^pub struct (\w+)/gm)].map((match) => match[1]);
const uniqueStructs = [...new Set(structs)].sort();

const document = `# Protocol

Generated from \`crates/protocol/src/lib.rs\` by \`tools/protocol-doc.mjs\`.
Source digest: \`${digest}\`. Do not edit by hand; run the generator.

One WebSocket frame is one message: the first byte is the tag, the rest is a
postcard payload. Frames are capped at ${source.match(/MAX_FRAME_BYTES: usize = ([\d_]+)/)?.[1]?.replaceAll("_", "") ?? "4096"} bytes.

## Constants

${constants}

## Client to server

${table(c2s)}

## Server to client

${table(s2c)}

## Payload types

${uniqueStructs.map((name) => `- \`${name}\``).join("\n")}

The field-level authority is the Rust struct above each tag. Validation rules
and size budgets live in \`crates/protocol/src/codec.rs\` and in the protocol
tests; this page is an index, not a second implementation.
`;

writeFileSync("docs/PROTOCOL.md", document);
console.log(`wrote docs/PROTOCOL.md (${c2s.length} C2S, ${s2c.length} S2C, digest ${digest})`);
