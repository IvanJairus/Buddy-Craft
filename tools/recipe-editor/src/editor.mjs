/**
 * Parse, edit and validate `data/recipes.ron` for the local recipe editor.
 *
 * The validation mirrors the load-time rules in `crates/sim/src/recipe.rs`:
 * minimum and maximum cluster size, pad bounds, ground support, a single
 * 6-connected component, and no duplicate canonical form under the four yaw
 * rotations. The editor only rewrites the block arrays, so comments and every
 * metadata field stay byte-for-byte intact.
 */

export const PAD_SIZE = 8;
export const PAD_HEIGHT = 4;
export const MIN_CLUSTER = 4;
export const MAX_CLUSTER = 32;
export const LAYERS = PAD_HEIGHT;

const SUPPORTED_OUTPUTS = new Set([
  "Sword",
  "Bow",
  "Bandage",
  "Spear",
  "Tower",
  "Ram",
  "Catapult",
  "Ward",
  "Buddy(Swordsman)",
  "Buddy(Archer)",
  "Buddy(Mage)",
  "Buddy(Healer)",
  "Buddy(Berserker)",
  "Buddy(Sniper)",
]);

const ID_RE = /\bid:\s*"([^"]+)"/g;
const BLOCKS_RE = /\bblocks:\s*\[/;
const TRIPLE_RE = /\[\s*(-?\d+)\s*,\s*(-?\d+)\s*,\s*(-?\d+)\s*\]/g;

function withoutComments(text) {
  return text.replace(/\/\/[^\n]*/g, "");
}

/** Index of the `]` matching the `[` at `open`, ignoring comments. */
function findMatchingBracket(source, open) {
  let depth = 0;
  for (let i = open; i < source.length; i += 1) {
    if (source[i] === "/" && source[i + 1] === "/") {
      const newline = source.indexOf("\n", i);
      if (newline === -1) break;
      i = newline;
      continue;
    }
    if (source[i] === "[") depth += 1;
    else if (source[i] === "]") {
      depth -= 1;
      if (depth === 0) return i;
    }
  }
  throw new Error("unterminated blocks array");
}

/** Every recipe in file order, with the source span of its block list. */
export function parseRecipes(source) {
  const ids = [...source.matchAll(ID_RE)];
  return ids.map((match, index) => {
    const start = match.index;
    const end = index + 1 < ids.length ? ids[index + 1].index : source.length;
    const segment = source.slice(start, end);
    const clean = withoutComments(segment);
    const blocksMatch = BLOCKS_RE.exec(segment);
    if (blocksMatch === null) {
      throw new Error(`recipe ${match[1]} has no blocks array`);
    }
    const open = start + blocksMatch.index + blocksMatch[0].lastIndexOf("[");
    const close = findMatchingBracket(source, open);
    const blocks = [...withoutComments(source.slice(open + 1, close)).matchAll(TRIPLE_RE)].map(
      (triple) => [Number(triple[1]), Number(triple[2]), Number(triple[3])],
    );
    const output = /\boutput:\s*([A-Za-z]+)(?:\(([A-Za-z]+)\))?/.exec(clean);
    const unlock = /\bunlock_s:\s*(\d+)/.exec(clean);
    const cooldown = /\bcooldown_s:\s*(\d+)/.exec(clean);
    return {
      id: match[1],
      output: output === null ? "" : output[2] ? `${output[1]}(${output[2]})` : output[1],
      unlock_s: unlock === null ? 0 : Number(unlock[1]),
      cooldown_s: cooldown === null ? 0 : Number(cooldown[1]),
      blocks,
      blocksContentStart: open + 1,
      blocksContentEnd: close,
    };
  });
}

function comparePosition(a, b) {
  return a[1] - b[1] || a[2] - b[2] || a[0] - b[0];
}

function normalise(blocks) {
  if (blocks.length === 0) return [];
  const min = [Infinity, Infinity, Infinity];
  for (const block of blocks) {
    for (let axis = 0; axis < 3; axis += 1) {
      min[axis] = Math.min(min[axis], block[axis]);
    }
  }
  return blocks
    .map((block) => [block[0] - min[0], block[1] - min[1], block[2] - min[2]])
    .sort(comparePosition);
}

function rotateY(blocks, turns) {
  return blocks.map((block) => {
    const [x, y, z] = block;
    switch (turns % 4) {
      case 1:
        return [-z, y, x];
      case 2:
        return [-x, y, -z];
      case 3:
        return [z, y, -x];
      default:
        return [x, y, z];
    }
  });
}

function packShape(blocks) {
  return blocks.map((block) => block.join(",")).join(";");
}

/**
 * Rotation-invariant identity of a shape. Equality is all that matters, so the
 * smallest packed rotation is chosen under a stable string order.
 */
export function canonicalKey(blocks) {
  if (blocks.length === 0) return "";
  const base = normalise(blocks);
  let best = null;
  for (let turn = 0; turn < 4; turn += 1) {
    const key = packShape(normalise(rotateY(base, turn)));
    if (best === null || key < best) best = key;
  }
  return best;
}

function isConnected(blocks) {
  if (blocks.length === 0) return false;
  const occupied = new Set(blocks.map((block) => block.join(",")));
  const seen = new Set();
  const stack = [blocks[0]];
  seen.add(blocks[0].join(","));
  const neighbours = [
    [1, 0, 0],
    [-1, 0, 0],
    [0, 1, 0],
    [0, -1, 0],
    [0, 0, 1],
    [0, 0, -1],
  ];
  while (stack.length > 0) {
    const [x, y, z] = stack.pop();
    for (const [dx, dy, dz] of neighbours) {
      const key = `${x + dx},${y + dy},${z + dz}`;
      if (occupied.has(key) && !seen.has(key)) {
        seen.add(key);
        stack.push([x + dx, y + dy, z + dz]);
      }
    }
  }
  return seen.size === blocks.length;
}

/** Every load-time rule the editor can enforce without the Rust crate. */
export function validateRecipes(recipes) {
  const errors = [];
  const ids = new Set();
  const canonical = new Map();
  recipes.forEach((recipe, index) => {
    const name = recipe.id || `#${index + 1}`;
    const fail = (message) => errors.push({ recipe: name, message });
    if (!recipe.id) fail("id is empty");
    else if (ids.has(recipe.id)) fail(`duplicate id "${recipe.id}"`);
    ids.add(recipe.id);
    if (!SUPPORTED_OUTPUTS.has(recipe.output)) {
      fail(`unsupported output "${recipe.output || "(empty)"}"`);
    }
    const blocks = recipe.blocks;
    if (!Array.isArray(blocks)) {
      fail("blocks is not an array");
      return;
    }
    if (blocks.length < MIN_CLUSTER) {
      fail(`${blocks.length} blocks is below the ${MIN_CLUSTER}-block minimum`);
    }
    if (blocks.length > MAX_CLUSTER) {
      fail(`${blocks.length} blocks exceeds the ${MAX_CLUSTER}-block cap`);
    }
    const seen = new Set();
    let coordinatesValid = true;
    for (const block of blocks) {
      if (
        !Array.isArray(block) ||
        block.length !== 3 ||
        !block.every((value) => Number.isInteger(value))
      ) {
        fail(`block ${JSON.stringify(block)} is not an integer triple`);
        coordinatesValid = false;
        continue;
      }
      const [x, y, z] = block;
      if (x < 0 || x >= PAD_SIZE || z < 0 || z >= PAD_SIZE || y < 0 || y >= PAD_HEIGHT) {
        fail(`block [${x},${y},${z}] is outside the ${PAD_SIZE}x${PAD_HEIGHT}x${PAD_SIZE} pad`);
        coordinatesValid = false;
      }
      const key = block.join(",");
      if (seen.has(key)) fail(`duplicate coordinate [${key}]`);
      seen.add(key);
    }
    if (!coordinatesValid || blocks.length === 0) return;
    const minY = Math.min(...blocks.map((block) => block[1]));
    if (minY !== 0) {
      fail(`lowest layer is y = ${minY}; it must be y = 0 to rest on the pad floor`);
    }
    if (!isConnected(blocks)) {
      fail("shape is not one 6-connected component");
    }
    if (blocks.length >= MIN_CLUSTER) {
      const key = canonicalKey(blocks);
      const other = canonical.get(key);
      if (other !== undefined) {
        fail(`canonical form collides with "${other}"`);
      } else {
        canonical.set(key, recipe.id);
      }
    }
  });
  return errors;
}

function formatBlocks(blocks) {
  if (blocks.length === 0) return "";
  return [...blocks]
    .sort(comparePosition)
    .map((block) => `      [${block[0]},${block[1]},${block[2]}],`)
    .join("\n");
}

/**
 * Rewrite one recipe's block array. Everything outside that array, including
 * the comment block above it, is returned unchanged.
 */
export function replaceRecipeBlocks(source, id, blocks) {
  const recipes = parseRecipes(source);
  const recipe = recipes.find((candidate) => candidate.id === id);
  if (recipe === undefined) throw new Error(`unknown recipe "${id}"`);
  const formatted = formatBlocks(blocks);
  const content = formatted.length > 0 ? `\n${formatted}\n    ` : "\n    ";
  return (
    source.slice(0, recipe.blocksContentStart) +
    content +
    source.slice(recipe.blocksContentEnd)
  );
}

function assertEdits(edits) {
  if (!Array.isArray(edits)) throw new Error("edits must be an array");
  for (const edit of edits) {
    if (edit === null || typeof edit !== "object" || typeof edit.id !== "string") {
      throw new Error("each edit must have a string id");
    }
    if (!Array.isArray(edit.blocks)) {
      throw new Error(`edit ${edit.id} has no blocks array`);
    }
    for (const block of edit.blocks) {
      if (
        !Array.isArray(block) ||
        block.length !== 3 ||
        !block.every((value) => Number.isInteger(value))
      ) {
        throw new Error(`edit ${edit.id} contains a non-integer triple`);
      }
    }
  }
}

/** Apply every edit, then return the rewritten source. Validation is separate. */
export function applyEdits(source, edits) {
  assertEdits(edits);
  let next = source;
  for (const edit of edits) {
    next = replaceRecipeBlocks(next, edit.id, edit.blocks);
  }
  return next;
}

export function validateSource(source) {
  const recipes = parseRecipes(source);
  return { recipes, errors: validateRecipes(recipes) };
}
