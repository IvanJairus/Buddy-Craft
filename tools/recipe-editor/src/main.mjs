import { LAYERS, validateRecipes } from "./editor.mjs";

const list = document.querySelector("#recipes");
const status = document.querySelector("#status");
const message = document.querySelector("#message");
const saveButton = document.querySelector("#save");
const resetButton = document.querySelector("#reset");

const state = {
  recipes: [],
  source: "",
  dirty: new Set(),
  layers: new Map(),
  errors: [],
  saving: false,
};

function cloneRecipes(recipes) {
  return recipes.map((recipe) => ({
    ...recipe,
    blocks: recipe.blocks.map((block) => [...block]),
  }));
}

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className !== undefined) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function setMessage(text, kind = "") {
  message.textContent = text;
  message.className = `message${kind ? ` ${kind}` : ""}`;
}

function outputLabel(output) {
  return output.replace(/^Buddy\((.+)\)$/, "$1");
}

function clock(seconds) {
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

function tierLabel(seconds) {
  const tier = seconds >= 480 ? 3 : seconds >= 180 ? 2 : 1;
  return `tier ${tier} · ${clock(seconds)}`;
}

function layerFor(recipe) {
  return state.layers.get(recipe.id) ?? 0;
}

function blocksAt(recipe, layer) {
  return recipe.blocks.filter((block) => block[1] === layer).length;
}

function hasBlock(recipe, x, y, z) {
  return recipe.blocks.some((block) => block[0] === x && block[1] === y && block[2] === z);
}

function toggleBlock(recipe, x, y, z) {
  const index = recipe.blocks.findIndex(
    (block) => block[0] === x && block[1] === y && block[2] === z,
  );
  if (index === -1) {
    recipe.blocks.push([x, y, z]);
  } else {
    recipe.blocks.splice(index, 1);
  }
  state.dirty.add(recipe.id);
}

function errorsFor(recipe) {
  return state.errors.filter((error) => error.recipe === recipe.id);
}

function makeLayerTabs(recipe) {
  const row = el("div", "layers");
  row.setAttribute("role", "group");
  row.setAttribute("aria-label", `${recipe.id} layers`);
  for (let layer = 0; layer < LAYERS; layer += 1) {
    const button = el("button", "layer", `y${layer}`);
    button.type = "button";
    button.dataset.layer = String(layer);
    button.setAttribute("aria-pressed", String(layerFor(recipe) === layer));
    row.append(button);
  }
  return row;
}

function makeGrid(recipe) {
  const layer = layerFor(recipe);
  const grid = el("div", "grid");
  grid.setAttribute("role", "grid");
  grid.setAttribute("aria-label", `${recipe.id} layer y${layer}`);
  for (let z = 0; z < 8; z += 1) {
    for (let x = 0; x < 8; x += 1) {
      const filled = hasBlock(recipe, x, layer, z);
      const cell = el("button", "cell");
      cell.type = "button";
      cell.dataset.x = String(x);
      cell.dataset.y = String(layer);
      cell.dataset.z = String(z);
      cell.setAttribute("aria-pressed", String(filled));
      cell.setAttribute("aria-label", `x${x} y${layer} z${z}: ${filled ? "crate" : "empty"}`);
      cell.title = `(${x}, ${layer}, ${z})`;
      grid.append(cell);
    }
  }
  return grid;
}

function makeCard(recipe) {
  const card = el("article", "recipe-card");
  card.dataset.recipe = recipe.id;
  const errors = errorsFor(recipe);
  if (errors.length > 0) card.classList.add("invalid");

  const head = el("div", "recipe-head");
  head.append(el("h2", "recipe-name", recipe.id), el("span", "output", outputLabel(recipe.output)));

  const meta = el(
    "div",
    "meta",
    `${recipe.blocks.length} crates · cooldown ${recipe.cooldown_s}s · ${tierLabel(recipe.unlock_s)}`,
  );
  const layerLabel = el(
    "span",
    "layer-count",
    `layer y${layerFor(recipe)} · ${blocksAt(recipe, layerFor(recipe))} crates`,
  );
  const axis = el("p", "axis", "x → 0..7, z ↓ 0..7. y0 is the pad floor.");
  axis.append(layerLabel);

  const errorList = el("ul", "errors");
  for (const error of errors) {
    errorList.append(el("li", undefined, error.message));
  }

  card.append(head, meta, makeLayerTabs(recipe), makeGrid(recipe), axis, errorList);
  return card;
}

function render() {
  const previous = document.activeElement;
  const focus =
    previous instanceof HTMLElement && previous.dataset.x !== undefined
      ? {
          recipe: previous.closest(".recipe-card")?.dataset.recipe,
          x: previous.dataset.x,
          y: previous.dataset.y,
          z: previous.dataset.z,
        }
      : null;

  list.replaceChildren(...state.recipes.map(makeCard));
  status.textContent =
    `${state.recipes.length} recipes · ${state.dirty.size} unsaved · ` +
    `${state.errors.length} ${state.errors.length === 1 ? "error" : "errors"}`;
  saveButton.disabled = state.saving || state.dirty.size === 0 || state.errors.length > 0;
  saveButton.textContent = state.saving ? "Saving..." : "Save changes";

  if (focus !== null) {
    const selector =
      `.recipe-card[data-recipe="${focus.recipe}"] ` +
      `button.cell[data-x="${focus.x}"][data-y="${focus.y}"][data-z="${focus.z}"]`;
    list.querySelector(selector)?.focus();
  }
}

async function load() {
  const response = await fetch("/api/recipes");
  if (!response.ok) throw new Error(`load failed with ${response.status}`);
  const body = await response.json();
  state.recipes = cloneRecipes(body.recipes);
  state.source = body.source;
  state.dirty.clear();
  state.layers = new Map(state.recipes.map((recipe) => [recipe.id, 0]));
  state.errors = validateRecipes(state.recipes);
  setMessage(
    state.errors.length === 0
      ? "Table loaded. Changes are saved to data/recipes.ron only after validation."
      : `Loaded table has ${state.errors.length} validation error(s).`,
    state.errors.length === 0 ? "" : "error",
  );
  render();
}

async function save() {
  if (state.saving) return;
  const edits = state.recipes
    .filter((recipe) => state.dirty.has(recipe.id))
    .map((recipe) => ({ id: recipe.id, blocks: recipe.blocks }));
  if (edits.length === 0) return;
  state.saving = true;
  render();
  try {
    const response = await fetch("/api/recipes", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ edits }),
    });
    const body = await response.json();
    if (!response.ok) {
      state.errors = Array.isArray(body.errors)
        ? body.errors
        : [{ recipe: "server", message: body.error ?? `save failed with ${response.status}` }];
      setMessage(`Save refused: ${state.errors[0]?.message ?? "invalid table"}`, "error");
      return;
    }
    state.recipes = cloneRecipes(body.recipes);
    state.source = body.source;
    state.dirty.clear();
    state.errors = [];
    state.layers = new Map(
      [...state.layers].filter(([id]) => state.recipes.some((recipe) => recipe.id === id)),
    );
    setMessage(`Saved ${edits.length} recipe(s) to data/recipes.ron.`, "ok");
  } catch (error) {
    setMessage(`Save failed: ${error instanceof Error ? error.message : String(error)}`, "error");
  } finally {
    state.saving = false;
    render();
  }
}

list.addEventListener("click", (event) => {
  const target = event.target;
  if (!(target instanceof HTMLElement)) return;
  const card = target.closest(".recipe-card");
  if (card === null) return;
  const recipe = state.recipes.find((candidate) => candidate.id === card.dataset.recipe);
  if (recipe === undefined) return;

  const layerButton = target.closest("button[data-layer]");
  if (layerButton !== null) {
    state.layers.set(recipe.id, Number(layerButton.dataset.layer));
    render();
    return;
  }

  const cell = target.closest("button.cell");
  if (cell === null) return;
  toggleBlock(recipe, Number(cell.dataset.x), Number(cell.dataset.y), Number(cell.dataset.z));
  state.errors = validateRecipes(state.recipes);
  setMessage("");
  render();
});

saveButton.addEventListener("click", () => {
  void save();
});

resetButton.addEventListener("click", () => {
  if (state.dirty.size > 0 && !window.confirm("Discard unsaved changes?")) return;
  load().catch((error) => setMessage(`Reset failed: ${error.message}`, "error"));
});

window.addEventListener("beforeunload", (event) => {
  if (state.dirty.size === 0) return;
  event.preventDefault();
  event.returnValue = "";
});

load().catch((error) => {
  setMessage(`Load failed: ${error.message}. Start the editor with node tools/recipe-editor/server.mjs.`, "error");
});
