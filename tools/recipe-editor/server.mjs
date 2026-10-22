#!/usr/bin/env node
/**
 * Local-only recipe editor server.
 *
 * Serves the editor UI on 127.0.0.1 and exposes the canonical
 * `data/recipes.ron` through two routes. A save rewrites only the edited block
 * arrays and is refused unless the whole table passes the same load-time rules
 * the Rust crate enforces. The file is replaced atomically through a temp file.
 */

import { createServer } from "node:http";
import { readFileSync, renameSync, writeFileSync } from "node:fs";
import { dirname, extname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { applyEdits, parseRecipes, validateRecipes } from "./src/editor.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const DEFAULT_RECIPES = join(HERE, "..", "..", "data", "recipes.ron");
const BODY_LIMIT = 2 * 1024 * 1024;
const STATIC = new Map([
  ["/", "index.html"],
  ["/index.html", "index.html"],
  ["/style.css", "style.css"],
  ["/src/main.mjs", "src/main.mjs"],
  ["/src/editor.mjs", "src/editor.mjs"],
]);
const TYPES = new Map([
  [".html", "text/html; charset=utf-8"],
  [".css", "text/css; charset=utf-8"],
  [".mjs", "text/javascript; charset=utf-8"],
]);

function log(message) {
  console.log(`recipe-editor: ${message}`);
}

function sendJson(response, status, body) {
  response.writeHead(status, {
    "Content-Type": "application/json; charset=utf-8",
    "Cache-Control": "no-store",
  });
  response.end(JSON.stringify(body));
}

async function readJson(request) {
  let size = 0;
  const chunks = [];
  for await (const chunk of request) {
    size += chunk.length;
    if (size > BODY_LIMIT) {
      throw new Error("request body exceeds 2 MB");
    }
    chunks.push(chunk);
  }
  if (chunks.length === 0) throw new Error("request body is empty");
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

function saveEdits(recipesPath, edits) {
  const source = readFileSync(recipesPath, "utf8");
  const next = applyEdits(source, edits);
  const recipes = parseRecipes(next);
  const errors = validateRecipes(recipes);
  if (errors.length > 0) {
    log(`save refused: ${errors.length} validation error(s), first ${errors[0].recipe}: ${errors[0].message}`);
    return { ok: false, errors };
  }
  const temporary = `${recipesPath}.tmp`;
  writeFileSync(temporary, next);
  renameSync(temporary, recipesPath);
  log(`saved ${edits.length} edited recipe(s), ${recipes.length} total`);
  return { ok: true, source: next, recipes, errors: [] };
}

export function startServer(options = {}) {
  const recipesPath = options.recipesPath ?? DEFAULT_RECIPES;
  const port = options.port ?? 0;
  const server = createServer(async (request, response) => {
    const url = new URL(request.url ?? "/", "http://127.0.0.1");
    try {
      if (url.pathname === "/api/recipes" && request.method === "GET") {
        const source = readFileSync(recipesPath, "utf8");
        sendJson(response, 200, { source, recipes: parseRecipes(source) });
        log("served recipe table");
        return;
      }
      if (url.pathname === "/api/recipes" && request.method === "PUT") {
        const body = await readJson(request);
        const result = saveEdits(recipesPath, body.edits);
        sendJson(response, result.ok ? 200 : 400, result);
        return;
      }
      const file = STATIC.get(url.pathname);
      if (file !== undefined && request.method === "GET") {
        const body = readFileSync(join(HERE, file));
        response.writeHead(200, {
          "Content-Type": TYPES.get(extname(file)) ?? "application/octet-stream",
          "Cache-Control": "no-store",
        });
        response.end(body);
        return;
      }
      sendJson(response, 404, { error: "not found" });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      log(`request failed: ${request.method} ${url.pathname}: ${message}`);
      sendJson(response, 400, { error: message });
    }
  });
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, "127.0.0.1", () => {
      const address = server.address();
      const bound = typeof address === "object" && address !== null ? address.port : port;
      resolve({ server, port: bound, url: `http://127.0.0.1:${bound}` });
    });
  });
}

const direct = process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href;
if (direct) {
  const requested = Number(process.env.RECIPE_EDITOR_PORT ?? 5174);
  const recipesPath = process.env.RECIPE_EDITOR_RECIPES ?? DEFAULT_RECIPES;
  const { url } = await startServer({
    port: Number.isFinite(requested) ? requested : 5174,
    recipesPath,
  });
  log(`listening on ${url}`);
  log(`editing ${recipesPath}`);
}
