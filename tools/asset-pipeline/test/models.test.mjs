import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const MODELS = join(ROOT, "apps", "web", "public", "assets", "models");
const EXPECTED = [
  "buddy_archer.glb",
  "buddy_berserker.glb",
  "buddy_healer.glb",
  "buddy_mage.glb",
  "buddy_sniper.glb",
  "buddy_swordsman.glb",
  "player_guard.glb",
  "player_raider.glb",
  "player_scout.glb",
];
const MAX_TRIS = 400;
const MAX_TOTAL = 40 * 1024;

const requireFromWeb = createRequire(join(ROOT, "apps", "web", "package.json"));
const { GLTFLoader } = await import(
  pathToFileURL(requireFromWeb.resolve("three/examples/jsm/loaders/GLTFLoader.js")).href
);

function glbJson(bytes) {
  assert.equal(bytes.readUInt32LE(0), 0x46546c67, "GLB magic");
  assert.equal(bytes.readUInt32LE(4), 2, "GLB version");
  assert.equal(bytes.readUInt32LE(8), bytes.length, "GLB total length");
  const jsonLength = bytes.readUInt32LE(12);
  assert.equal(bytes.readUInt32LE(16), 0x4e4f534a, "JSON chunk");
  return JSON.parse(bytes.toString("utf8", 20, 20 + jsonLength).trim());
}

function parseWithThree(bytes) {
  return new Promise((resolve, reject) => {
    const buffer = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
    new GLTFLoader().parse(buffer, "", resolve, reject);
  });
}

test("nine models match the T-051 triangle, colour and size budget", async () => {
  const files = readdirSync(MODELS).filter((file) => file.endsWith(".glb")).sort();
  assert.deepEqual(files, EXPECTED);

  let total = 0;
  for (const file of files) {
    const bytes = readFileSync(join(MODELS, file));
    total += bytes.length;
    const json = glbJson(bytes);
    assert.equal(json.textures, undefined, `${file} must not carry textures`);
    assert.equal(json.images, undefined, `${file} must not carry images`);
    assert.equal(json.materials, undefined, `${file} must not carry materials`);
    const primitive = json.meshes?.[0]?.primitives?.[0];
    assert.ok(primitive !== undefined, `${file} has no primitive`);
    assert.equal(primitive.mode, 4, `${file} must use triangles`);
    assert.ok(primitive.attributes.POSITION !== undefined, `${file} has no positions`);
    assert.ok(primitive.attributes.COLOR_0 !== undefined, `${file} has no vertex colours`);
    const colorAccessor = json.accessors[primitive.attributes.COLOR_0];
    assert.equal(colorAccessor.normalized, true, `${file} colours must be normalised`);
    assert.equal(colorAccessor.type, "VEC3");
    const indexAccessor = json.accessors[primitive.indices];
    const triangles = indexAccessor.count / 3;
    assert.ok(triangles < MAX_TRIS, `${file} has ${triangles} triangles`);

    const gltf = await parseWithThree(bytes);
    let parsedTriangles = 0;
    let parsedColours = false;
    gltf.scene.traverse((node) => {
      if (node.isMesh) {
        assert.equal(node.material.map ?? null, null, `${file} material has a texture`);
        parsedColours = parsedColours || node.geometry.getAttribute("color") !== undefined;
        const index = node.geometry.getIndex();
        parsedTriangles += index === null ? node.geometry.getAttribute("position").count / 3 : index.count / 3;
      }
    });
    assert.equal(parsedTriangles, triangles, `${file} three.js triangle count`);
    assert.ok(parsedColours, `${file} lost its vertex colours in three.js`);
  }
  assert.ok(total <= MAX_TOTAL, `models total ${total} bytes, budget is ${MAX_TOTAL}`);
});
