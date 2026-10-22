import assert from "node:assert/strict";
import { copyFileSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import {
  applyEdits,
  canonicalKey,
  parseRecipes,
  replaceRecipeBlocks,
  validateRecipes,
  validateSource,
} from "../src/editor.mjs";
import { startServer } from "../server.mjs";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const RECIPES = join(ROOT, "data", "recipes.ron");
const SOURCE = readFileSync(RECIPES, "utf8");

function rotate(blocks) {
  return blocks.map(([x, y, z]) => [-z, y, x]);
}

function anchor(blocks) {
  const minX = Math.min(...blocks.map(([x]) => x));
  const minZ = Math.min(...blocks.map(([, , z]) => z));
  return blocks.map(([x, y, z]) => [x - minX, y, z - minZ]);
}

test("the shipped table parses and validates", () => {
  const recipes = parseRecipes(SOURCE);
  assert.equal(recipes.length, 14);
  assert.equal(recipes[0].id, "R01_SWORD");
  assert.equal(recipes[0].output, "Sword");
  assert.equal(recipes[0].blocks.length, 6);
  assert.equal(recipes[5].id, "R06_MAGE");
  assert.equal(recipes[5].output, "Buddy(Mage)");
  assert.equal(recipes[13].blocks.length, 15);
  assert.deepEqual(validateRecipes(recipes), []);
});

test("rotation is the same canonical form and a mirror is not", () => {
  const sword = parseRecipes(SOURCE)[0].blocks;
  assert.equal(canonicalKey(rotate(sword)), canonicalKey(sword));
  const zigzag = [
    [0, 0, 0],
    [1, 0, 0],
    [1, 0, 1],
    [2, 0, 1],
  ];
  const mirrored = zigzag.map(([x, y, z]) => [-x, y, z]);
  assert.notEqual(canonicalKey(mirrored), canonicalKey(zigzag));
});

test("editing blocks preserves comments and metadata", () => {
  const recipe = parseRecipes(SOURCE)[2];
  const moved = recipe.blocks.map(([x, y, z]) => [x + 1, y, z + 1]);
  const edited = replaceRecipeBlocks(SOURCE, recipe.id, moved);
  assert.match(edited, /\/\/ Cratebound recipe table\./);
  assert.match(edited, /\/\/ --- Tier 1/);
  assert.match(edited, /cooldown_s: 15/);
  const reread = parseRecipes(edited);
  assert.equal(reread.length, 14);
  assert.deepEqual(reread[2].blocks, moved);
  assert.deepEqual(validateRecipes(reread), []);
});

test("a rotated duplicate is refused", () => {
  const recipes = parseRecipes(SOURCE);
  const sword = recipes[0].blocks;
  const edits = [{ id: "R02_BOW", blocks: anchor(rotate(sword)) }];
  const { errors } = validateSource(applyEdits(SOURCE, edits));
  assert.ok(errors.some((error) => /canonical form collides/.test(error.message)));
});

test("unsupported shapes are refused", () => {
  const sword = parseRecipes(SOURCE)[0].blocks;
  const cases = [
    { name: "below minimum", blocks: sword.slice(0, 3) },
    {
      name: "above maximum",
      blocks: Array.from({ length: 33 }, (_, i) => [i % 8, 0, Math.floor(i / 8)]),
    },
    { name: "above the floor", blocks: sword.map(([x, y, z]) => [x, y + 1, z]) },
    { name: "duplicate coordinate", blocks: [...sword, sword[0]] },
    {
      name: "disconnected",
      blocks: [
        [0, 0, 0],
        [1, 0, 0],
        [2, 0, 0],
        [4, 0, 0],
      ],
    },
    { name: "out of bounds", blocks: sword.map(([x, y, z]) => (x === 0 ? [8, y, z] : [x, y, z])) },
  ];
  for (const item of cases) {
    const errors = validateRecipes([{ id: "TEST", output: "Sword", blocks: item.blocks }]);
    assert.ok(errors.length > 0, `${item.name} should fail validation`);
  }
});

test("the save endpoint refuses invalid edits and keeps the file", async (t) => {
  const directory = mkdtempSync(join(tmpdir(), "recipe-editor-"));
  const recipesPath = join(directory, "recipes.ron");
  copyFileSync(RECIPES, recipesPath);
  const before = readFileSync(recipesPath, "utf8");
  const { server, url } = await startServer({ recipesPath, port: 0 });
  t.after(() => server.close());

  const loaded = await fetch(`${url}/api/recipes`).then((response) => response.json());
  assert.equal(loaded.recipes.length, 14);

  const sword = loaded.recipes[0].blocks;
  const duplicate = await fetch(`${url}/api/recipes`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ edits: [{ id: "R02_BOW", blocks: anchor(rotate(sword)) }] }),
  });
  assert.equal(duplicate.status, 400);
  const duplicateBody = await duplicate.json();
  assert.ok(duplicateBody.errors.some((error) => /canonical form collides/.test(error.message)));
  assert.equal(readFileSync(recipesPath, "utf8"), before);

  const floating = await fetch(`${url}/api/recipes`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      edits: [{ id: "R01_SWORD", blocks: sword.map(([x, y, z]) => [x, y + 1, z]) }],
    }),
  });
  assert.equal(floating.status, 400);
  assert.equal(readFileSync(recipesPath, "utf8"), before);

  const square = loaded.recipes[2].blocks.map(([x, y, z]) => [x + 1, y, z + 1]);
  const accepted = await fetch(`${url}/api/recipes`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ edits: [{ id: "R03_BANDAGE", blocks: square }] }),
  });
  assert.equal(accepted.status, 200);
  const saved = await accepted.json();
  assert.equal(saved.recipes[2].blocks.length, 4);
  const written = readFileSync(recipesPath, "utf8");
  assert.notEqual(written, before);
  assert.match(written, /\/\/ Cratebound recipe table\./);
  assert.deepEqual(parseRecipes(written)[2].blocks, square);
});
