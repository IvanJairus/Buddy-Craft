/**
 * Desktop input (task T-035).
 *
 * The module owns the browser's input state and packs it into the 4-byte wire
 * frame of §7.1: buttons, yaw, pitch and two signed 4-bit axes. The game loop
 * calls [`DesktopInput.poll`] exactly once per frame and hands the returned
 * array to `cb_tick`; the class counts those polls so the integration test can
 * assert there is one pack per tick.
 *
 * # Pointer lock
 *
 * The first click on the canvas requests pointer lock and is not an action.
 * Mouse look and mouse buttons only apply while the pointer is locked. Losing
 * focus releases every key and button: the "player runs forever after alt-tab"
 * bug is a missing blur handler, not a simulation bug.
 *
 * # Settings
 *
 * Sensitivity and invert-Y live in localStorage, read at construction and
 * written on every change. The defaults are 0.25 yaw units per pixel and
 * non-inverted; the tests persist 0.5/true across a reload.
 */

export const JUMP = 1 << 0;
export const FIRE = 1 << 1;
export const USE = 1 << 2;
export const STANCE = 1 << 3;
export const CROUCH = 1 << 4;

export const SENSITIVITY_KEY = "buddycraft.sensitivity";
export const INVERT_Y_KEY = "buddycraft.invertY";
export const DEFAULT_SENSITIVITY = 0.25;
export const MIN_SENSITIVITY = 0.02;
export const MAX_SENSITIVITY = 2;

const MOVE_KEYS = new Set([
  "KeyW",
  "KeyA",
  "KeyS",
  "KeyD",
  "ArrowUp",
  "ArrowLeft",
  "ArrowDown",
  "ArrowRight",
]);

export type InputAction =
  | "drop"
  | "scoreboard"
  | "recipes"
  | "stance"
  | "menu"
  | "slot";

export interface InputSettingsStore {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

export interface DesktopInputOptions {
  canvas: HTMLCanvasElement;
  storage?: InputSettingsStore;
  window?: Window;
  document?: Document;
}

/** The state that becomes one 4-byte frame. Exported for tests. */
export interface PackedState {
  forward: number;
  strafe: number;
  jump: boolean;
  fire: boolean;
  use: boolean;
  stance: boolean;
  crouch: boolean;
  yaw: number;
  pitch: number;
}

function axisNibble(value: number): number {
  if (value > 0) {
    return 7;
  }
  if (value < 0) {
    return 8;
  }
  return 0;
}

/** Pack the state into the §7.1 frame. Axes are 4-bit two's complement. */
export function packFrame(state: PackedState, out = new Uint8Array(4)): Uint8Array {
  let buttons = 0;
  if (state.jump) buttons |= JUMP;
  if (state.fire) buttons |= FIRE;
  if (state.use) buttons |= USE;
  if (state.stance) buttons |= STANCE;
  if (state.crouch) buttons |= CROUCH;
  out[0] = buttons;
  out[1] = Math.round(state.yaw) & 0xff;
  out[2] = Math.round(state.pitch) & 0xff;
  out[3] = (axisNibble(state.forward) << 4) | axisNibble(state.strafe);
  return out;
}

function clamp(value: number, low: number, high: number): number {
  return Math.max(low, Math.min(high, value));
}

export class DesktopInput {
  private readonly canvas: HTMLCanvasElement;
  private readonly window: Window;
  private readonly document: Document;
  private readonly storage: InputSettingsStore;
  private readonly held = new Set<string>();
  private readonly frame = new Uint8Array(4);
  private pointerLocked = false;
  private left = false;
  private right = false;
  private primaryPressed = false;
  private secondaryPressed = false;
  private yaw = 0;
  private pitch = 0;
  private sensitivity = DEFAULT_SENSITIVITY;
  private invertY = false;
  private slot = 0;
  private pollCount = 0;
  private lastAction: InputAction | null = null;
  private onActionCallback: ((action: InputAction, value?: number) => void) | null = null;

  private readonly onCanvasClick = (): void => {
    if (!this.pointerLocked) {
      const request = this.canvas.requestPointerLock() as unknown;
      if (request instanceof Promise) {
        void request.catch(() => {
          // A browser without pointer lock still gets keyboard and edit input;
          // failing to lock must not become an unhandled rejection.
        });
      }
    }
  };
  private readonly onPointerLockChange = (): void => {
    this.pointerLocked = this.document.pointerLockElement === this.canvas;
    if (!this.pointerLocked) {
      this.releaseAll();
    }
  };
  private readonly onKeyDown = (event: KeyboardEvent): void => {
    if (event.code === "Tab") {
      event.preventDefault();
    }
    if (MOVE_KEYS.has(event.code)) {
      event.preventDefault();
      this.held.add(event.code);
      return;
    }
    if (event.code === "Space") {
      event.preventDefault();
      this.held.add(event.code);
      return;
    }
    if (event.repeat) {
      return;
    }
    switch (event.code) {
      case "Digit1":
      case "Digit2":
      case "Digit3":
      case "Digit4":
      case "Digit5":
      case "Digit6":
      case "Digit7":
      case "Digit8":
      case "Digit9": {
        const index = Number(event.code.slice(5)) - 1;
        this.setSlot(index);
        return;
      }
      case "KeyQ":
        this.emit("drop");
        return;
      case "Tab":
        this.emit("scoreboard");
        return;
      case "KeyE":
        this.emit("recipes");
        return;
      case "KeyF":
        this.held.add(event.code);
        this.emit("stance");
        return;
      case "Escape":
        this.emit("menu");
        return;
      default:
        return;
    }
  };
  private readonly onKeyUp = (event: KeyboardEvent): void => {
    if (event.code === "Tab") {
      event.preventDefault();
    }
    this.held.delete(event.code);
  };
  private readonly onMouseDown = (event: MouseEvent): void => {
    if (!this.pointerLocked) {
      return;
    }
    if (event.button === 0) {
      this.left = true;
      this.primaryPressed = true;
    } else if (event.button === 2) {
      this.right = true;
      this.secondaryPressed = true;
    }
  };
  private readonly onMouseUp = (event: MouseEvent): void => {
    if (event.button === 0) {
      this.left = false;
    } else if (event.button === 2) {
      this.right = false;
    }
  };
  private readonly onMouseMove = (event: MouseEvent): void => {
    if (!this.pointerLocked) {
      return;
    }
    const scale = this.sensitivity * (this.invertY ? 1 : -1);
    this.yaw =
      (((this.yaw + event.movementX * this.sensitivity) % 256) + 256) % 256;
    this.pitch = clamp(this.pitch + event.movementY * scale, -64, 64);
  };
  private readonly onWheel = (event: WheelEvent): void => {
    if (event.deltaY === 0) {
      return;
    }
    event.preventDefault();
    this.setSlot((this.slot + (event.deltaY > 0 ? 1 : -1) + 9) % 9);
  };
  private readonly onContextMenu = (event: MouseEvent): void => {
    if (this.pointerLocked) {
      event.preventDefault();
    }
  };
  private readonly onBlur = (): void => this.releaseAll();

  constructor(options: DesktopInputOptions) {
    this.canvas = options.canvas;
    this.window = options.window ?? window;
    this.document = options.document ?? document;
    this.storage = options.storage ?? window.localStorage;

    const storedSensitivity = Number.parseFloat(
      this.storage.getItem(SENSITIVITY_KEY) ?? "",
    );
    if (Number.isFinite(storedSensitivity)) {
      this.sensitivity = clamp(storedSensitivity, MIN_SENSITIVITY, MAX_SENSITIVITY);
    }
    this.invertY = this.storage.getItem(INVERT_Y_KEY) === "true";

    this.canvas.addEventListener("click", this.onCanvasClick);
    this.document.addEventListener("pointerlockchange", this.onPointerLockChange);
    this.window.addEventListener("keydown", this.onKeyDown);
    this.window.addEventListener("keyup", this.onKeyUp);
    this.window.addEventListener("mousedown", this.onMouseDown);
    this.window.addEventListener("mouseup", this.onMouseUp);
    this.document.addEventListener("mousemove", this.onMouseMove);
    this.window.addEventListener("wheel", this.onWheel, { passive: false });
    this.window.addEventListener("contextmenu", this.onContextMenu);
    this.window.addEventListener("blur", this.onBlur);
  }

  /** Fill and return the 4-byte frame. Call exactly once per tick. */
  poll(): Uint8Array {
    this.pollCount += 1;
    const forward = Number(this.held.has("KeyW") || this.held.has("ArrowUp")) -
      Number(this.held.has("KeyS") || this.held.has("ArrowDown"));
    const strafe = Number(this.held.has("KeyD") || this.held.has("ArrowRight")) -
      Number(this.held.has("KeyA") || this.held.has("ArrowLeft"));
    return packFrame(
      {
        forward,
        strafe,
        jump: this.held.has("Space"),
        fire: this.left,
        use: this.right,
        stance: this.held.has("KeyF"),
        crouch: false,
        yaw: this.yaw,
        pitch: this.pitch,
      },
      this.frame,
    );
  }

  /** A left click that the caller has not consumed yet. */
  consumePrimary(): boolean {
    const pressed = this.primaryPressed;
    this.primaryPressed = false;
    return pressed;
  }

  /** A right click that the caller has not consumed yet. */
  consumeSecondary(): boolean {
    const pressed = this.secondaryPressed;
    this.secondaryPressed = false;
    return pressed;
  }

  setSensitivity(value: number): void {
    this.sensitivity = clamp(value, MIN_SENSITIVITY, MAX_SENSITIVITY);
    this.storage.setItem(SENSITIVITY_KEY, String(this.sensitivity));
  }

  setInvertY(value: boolean): void {
    this.invertY = value;
    this.storage.setItem(INVERT_Y_KEY, value ? "true" : "false");
  }

  /** Set the hotbar slot, 0-based. */
  setSlot(index: number): void {
    this.slot = clamp(Math.floor(index), 0, 8);
    this.emit("slot", this.slot);
  }

  onAction(callback: (action: InputAction, value?: number) => void): void {
    this.onActionCallback = callback;
  }

  get selectedSlot(): number {
    return this.slot;
  }

  get pointerLock(): boolean {
    return this.pointerLocked;
  }

  get polls(): number {
    return this.pollCount;
  }

  get yawValue(): number {
    return this.yaw;
  }

  get pitchValue(): number {
    return this.pitch;
  }

  get sensitivityValue(): number {
    return this.sensitivity;
  }

  get invertYValue(): boolean {
    return this.invertY;
  }

  get lastActionValue(): InputAction | null {
    return this.lastAction;
  }

  /** The last frame `poll` produced, as numbers. For diagnostics and tests. */
  frameBytes(): number[] {
    return [this.frame[0], this.frame[1], this.frame[2], this.frame[3]];
  }

  /** Release every held key and button. Also runs on blur and unlock. */
  releaseAll(): void {
    this.held.clear();
    this.left = false;
    this.right = false;
    this.primaryPressed = false;
    this.secondaryPressed = false;
  }

  dispose(): void {
    this.canvas.removeEventListener("click", this.onCanvasClick);
    this.document.removeEventListener("pointerlockchange", this.onPointerLockChange);
    this.window.removeEventListener("keydown", this.onKeyDown);
    this.window.removeEventListener("keyup", this.onKeyUp);
    this.window.removeEventListener("mousedown", this.onMouseDown);
    this.window.removeEventListener("mouseup", this.onMouseUp);
    this.document.removeEventListener("mousemove", this.onMouseMove);
    this.window.removeEventListener("wheel", this.onWheel);
    this.window.removeEventListener("contextmenu", this.onContextMenu);
    this.window.removeEventListener("blur", this.onBlur);
    this.releaseAll();
  }

  private emit(action: InputAction, value?: number): void {
    this.lastAction = action;
    this.onActionCallback?.(action, value);
  }
}
