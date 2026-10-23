/**
 * Full map and objective readout. The map is north-up; the arrow rotates with
 * the camera. Base positions come from worldgen through `cb_bases`, so the map
 * does not duplicate the base-circle maths in TypeScript.
 */

import "./map.css";
import type { BaseRow } from "../game/objectives";

const WORLD = 256;
const SIZE = 384;
const INSET = 20;
const TEAM_COLORS = ["#6fd3ff", "#f08a7a", "#9fe0b0", "#ffd166", "#c9a0ff", "#ffa06b"];

export interface MapInfo {
  open: boolean;
  bases: number;
  yourTeam: number;
  player: [number, number];
  drop: [number, number] | null;
}

export class MapOverlay {
  readonly element: HTMLDivElement;
  private readonly canvas: HTMLCanvasElement;
  private readonly ctx: CanvasRenderingContext2D;
  private readonly status: HTMLDivElement;
  private bases: BaseRow[] = [];
  private yourTeam = 0;
  private px = WORLD / 2;
  private pz = WORLD / 2;
  private forwardX = 0;
  private forwardZ = -1;
  private drop: { x: number; z: number } | null = null;
  private open = false;
  private lastRender = 0;

  constructor(parent: HTMLElement, onClose?: () => void) {
    this.element = document.createElement("div");
    this.element.className = "cb-map";
    this.element.hidden = true;

    const panel = document.createElement("div");
    panel.className = "cb-map-panel";
    const head = document.createElement("div");
    head.className = "cb-map-head";
    const title = document.createElement("span");
    title.textContent = "MAP";
    const close = document.createElement("button");
    close.type = "button";
    close.className = "cb-map-close";
    close.textContent = "CLOSE (M)";
    close.addEventListener("click", () => {
      this.hide();
      onClose?.();
    });
    head.append(title, close);

    this.canvas = document.createElement("canvas");
    this.canvas.width = SIZE;
    this.canvas.height = SIZE;
    this.canvas.setAttribute("aria-label", "Top-down map of the arena");
    const context = this.canvas.getContext("2d");
    if (context === null) {
      throw new Error("2d canvas is unavailable");
    }
    this.ctx = context;

    const legend = document.createElement("div");
    legend.className = "cb-map-legend";
    for (const [color, label] of [
      ["#6fd3ff", "YOUR BASE"],
      ["#f08a7a", "ENEMY"],
      ["#8fd0ff", "PAD"],
      ["#ffd166", "DROP"],
      ["#f2f6fa", "YOU"],
    ]) {
      const item = document.createElement("span");
      const swatch = document.createElement("i");
      swatch.className = "cb-map-key";
      swatch.style.background = color;
      item.append(swatch, label);
      legend.append(item);
    }

    this.status = document.createElement("p");
    this.status.className = "cb-map-status";
    this.status.textContent = "WAITING FOR MATCH DATA";

    panel.append(head, this.canvas, legend, this.status);
    this.element.append(panel);
    parent.append(this.element);
  }

  setBases(bases: BaseRow[], yourTeam: number): void {
    this.bases = bases;
    this.yourTeam = yourTeam;
    if (this.open) this.draw();
  }

  setDrop(drop: { x: number; z: number } | null): void {
    this.drop = drop;
    if (this.open) this.draw();
  }

  updatePlayer(x: number, z: number, forwardX: number, forwardZ: number, now: number): void {
    this.px = x;
    this.pz = z;
    this.forwardX = forwardX;
    this.forwardZ = forwardZ;
    if (this.open && now - this.lastRender >= 80) {
      this.draw(now);
    }
  }

  get isOpen(): boolean {
    return this.open;
  }

  show(now = performance.now()): void {
    this.open = true;
    this.element.hidden = false;
    document.body.classList.add("cb-map-open");
    this.draw(now);
  }

  hide(): void {
    this.open = false;
    this.element.hidden = true;
    document.body.classList.remove("cb-map-open");
  }

  toggle(now = performance.now()): void {
    if (this.open) this.hide();
    else this.show(now);
  }

  info(): MapInfo {
    return {
      open: this.open,
      bases: this.bases.length,
      yourTeam: this.yourTeam,
      player: [this.px, this.pz],
      drop: this.drop === null ? null : [this.drop.x, this.drop.z],
    };
  }

  private draw(now = performance.now()): void {
    this.lastRender = now;
    const ctx = this.ctx;
    const scale = (SIZE - INSET * 2) / WORLD;
    const sx = (x: number): number => INSET + x * scale;
    const sy = (z: number): number => INSET + z * scale;

    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, SIZE, SIZE);
    ctx.fillStyle = "#0a1118";
    ctx.fillRect(0, 0, SIZE, SIZE);

    ctx.strokeStyle = "rgba(126, 156, 180, 0.16)";
    ctx.lineWidth = 1;
    for (let i = 0; i <= 8; i += 1) {
      const at = INSET + (i / 8) * (SIZE - INSET * 2);
      ctx.beginPath();
      ctx.moveTo(at, INSET);
      ctx.lineTo(at, SIZE - INSET);
      ctx.stroke();
      ctx.beginPath();
      ctx.moveTo(INSET, at);
      ctx.lineTo(SIZE - INSET, at);
      ctx.stroke();
    }

    for (let index = 0; index < this.bases.length; index += 1) {
      const base = this.bases[index];
      const own = index === this.yourTeam;
      const color = own ? TEAM_COLORS[0] : TEAM_COLORS[1 + (index % (TEAM_COLORS.length - 1))];
      ctx.fillStyle = own ? "rgba(111, 211, 255, 0.09)" : "rgba(240, 138, 122, 0.07)";
      ctx.beginPath();
      ctx.arc(sx(base.x), sy(base.z), 24 * scale, 0, Math.PI * 2);
      ctx.fill();
      ctx.strokeStyle = color;
      ctx.globalAlpha = 0.55;
      ctx.setLineDash([4, 4]);
      ctx.stroke();
      ctx.setLineDash([]);
      ctx.globalAlpha = 1;

      ctx.fillStyle = color;
      ctx.globalAlpha = 0.34;
      ctx.fillRect(sx(base.padX - 4), sy(base.padZ - 4), 8 * scale, 8 * scale);
      ctx.globalAlpha = 1;
      ctx.strokeStyle = color;
      ctx.strokeRect(sx(base.padX - 4) + 0.5, sy(base.padZ - 4) + 0.5, 8 * scale, 8 * scale);

      ctx.save();
      ctx.translate(sx(base.padX), sy(base.padZ));
      ctx.rotate(Math.PI / 4);
      ctx.fillStyle = color;
      ctx.fillRect(-3, -3, 6, 6);
      ctx.restore();

      ctx.fillStyle = color;
      ctx.font = "700 9px ui-monospace, monospace";
      ctx.fillText(own ? "YOU" : `E${index + 1}`, sx(base.padX) + 7, sy(base.padZ) - 6);
    }

    if (this.drop !== null) {
      const dx = sx(this.drop.x);
      const dz = sy(this.drop.z);
      ctx.strokeStyle = "#ffd166";
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.arc(dx, dz, 7, 0, Math.PI * 2);
      ctx.stroke();
      ctx.beginPath();
      ctx.moveTo(dx - 7, dz);
      ctx.lineTo(dx + 7, dz);
      ctx.moveTo(dx, dz - 7);
      ctx.lineTo(dx, dz + 7);
      ctx.stroke();
      ctx.fillStyle = "#ffd166";
      ctx.font = "700 9px ui-monospace, monospace";
      ctx.fillText("DROP", dx + 10, dz + 3);
    }

    ctx.save();
    ctx.translate(sx(this.px), sy(this.pz));
    ctx.rotate(Math.atan2(this.forwardX, -this.forwardZ));
    ctx.beginPath();
    ctx.moveTo(0, -8);
    ctx.lineTo(5.5, 6);
    ctx.lineTo(0, 3);
    ctx.lineTo(-5.5, 6);
    ctx.closePath();
    ctx.fillStyle = "#f2f6fa";
    ctx.fill();
    ctx.strokeStyle = "#081018";
    ctx.lineWidth = 1;
    ctx.stroke();
    ctx.restore();

    ctx.fillStyle = "#8095a8";
    ctx.font = "700 10px ui-monospace, monospace";
    ctx.fillText("N", SIZE / 2 - 4, 13);

    this.status.textContent =
      this.bases.length === 0
        ? "WAITING FOR MATCH DATA"
        : `${this.bases.length} BASES · M / ESC CLOSE · N UP`;
  }
}
