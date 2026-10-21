/**
 * Net client: control messages, reconnect and RTT (task T-040).
 *
 * The wire codec here covers the control messages this client needs today:
 * `C_HELLO` and `C_PING` out, and `S_WELCOME`, `S_ERROR` and `S_PONG` in.
 * Postcard is a varint format, so the encoder handles exactly those fields and
 * leaves every other server tag opaque for T-043's snapshot decoder.
 *
 * Reconnect policy is a pure `ReconnectBackoff`: 500 ms doubling to 8 s, ±20%
 * jitter, six attempts. The client resets it on a successful open and stops
 * entirely on a fatal server error. The reconnect token is stored as hex in
 * sessionStorage and presented in the next `C_HELLO`.
 */

import { WsTransport, type CloseInfo, type Transport } from "./transport";

export const NET_TOKEN_KEY = "buddycraft.netToken";
export const NET_STYLE_ID = "cb-net-style";
export const NAME_LEN = 16;
export const ROOM_CODE_LEN = 6;
export const TOKEN_LEN = 16;
export const ERROR_DETAIL_LEN = 64;

export const C2S_HELLO = 0x01;
export const C2S_PING = 0x08;
export const S2C_WELCOME = 0x81;
export const S2C_ERROR = 0x8a;
export const S2C_PONG = 0x8b;

export const ERROR_VERSION_MISMATCH = 1;

export type ServerMessage =
  | {
      kind: "welcome";
      playerId: number;
      team: number;
      seed: number;
      config: {
        mode: number;
        teams: number;
        perTeam: number;
        buddyCap: number;
        targetMinutes: number;
      };
      tick: number;
      token: Uint8Array;
    }
  | { kind: "error"; code: number; detail: string }
  | { kind: "pong"; t: number }
  | { kind: "other"; tag: number; payload: Uint8Array };

function pushVarint(out: number[], value: number): void {
  let rest = value >>> 0;
  do {
    let byte = rest & 0x7f;
    rest >>>= 7;
    if (rest !== 0) {
      byte |= 0x80;
    }
    out.push(byte);
  } while (rest !== 0);
}

function readVarint(bytes: Uint8Array, at: number): { value: number; next: number } {
  let value = 0;
  let shift = 0;
  let offset = at;
  while (offset < bytes.length) {
    const byte = bytes[offset];
    offset += 1;
    value += (byte & 0x7f) * 2 ** shift;
    if ((byte & 0x80) === 0) {
      return { value: value >>> 0, next: offset };
    }
    shift += 7;
    if (shift > 35) {
      break;
    }
  }
  throw new Error("truncated varint");
}

function ascii(text: string, length: number): Uint8Array {
  const out = new Uint8Array(length);
  for (let i = 0; i < Math.min(length, text.length); i += 1) {
    out[i] = text.charCodeAt(i) & 0x7f;
  }
  return out;
}

export interface HelloFields {
  protocol: number;
  name: string;
  roomCode: string;
  token: Uint8Array | null;
}

export function encodeHello(fields: HelloFields): Uint8Array {
  const out: number[] = [C2S_HELLO];
  pushVarint(out, fields.protocol);
  out.push(...ascii(fields.name, NAME_LEN));
  out.push(...ascii(fields.roomCode.toUpperCase(), ROOM_CODE_LEN));
  if (fields.token === null) {
    out.push(0);
  } else {
    out.push(1);
    const token = new Uint8Array(TOKEN_LEN);
    token.set(fields.token.subarray(0, TOKEN_LEN));
    out.push(...token);
  }
  return Uint8Array.from(out);
}

export function encodePing(t: number): Uint8Array {
  const out: number[] = [C2S_PING];
  pushVarint(out, t);
  return Uint8Array.from(out);
}

/** Test/debug helper: read the fields back out of a Hello frame. */
export function decodeHello(bytes: Uint8Array): HelloFields {
  if (bytes[0] !== C2S_HELLO) {
    throw new Error("not a Hello frame");
  }
  const protocol = readVarint(bytes, 1);
  let at = protocol.next;
  const name = new TextDecoder().decode(bytes.subarray(at, at + NAME_LEN)).replace(/\0.*$/, "");
  at += NAME_LEN;
  const roomCode = new TextDecoder()
    .decode(bytes.subarray(at, at + ROOM_CODE_LEN))
    .replace(/\0.*$/, "");
  at += ROOM_CODE_LEN;
  let token: Uint8Array | null = null;
  if (bytes[at] === 1) {
    at += 1;
    token = bytes.slice(at, at + TOKEN_LEN);
  }
  return { protocol: protocol.value, name, roomCode, token };
}

export function decodeS2c(data: ArrayBuffer | Uint8Array): ServerMessage {
  const bytes = data instanceof Uint8Array ? data : new Uint8Array(data);
  if (bytes.length === 0) {
    throw new Error("empty frame");
  }
  const tag = bytes[0];
  if (tag === S2C_WELCOME) {
    let at = 1;
    if (at + 2 > bytes.length) throw new Error("truncated Welcome");
    const playerId = bytes[at];
    const team = bytes[at + 1];
    at += 2;
    const seed = readVarint(bytes, at);
    at = seed.next;
    if (at + 5 > bytes.length) throw new Error("truncated Welcome config");
    const config = {
      mode: bytes[at],
      teams: bytes[at + 1],
      perTeam: bytes[at + 2],
      buddyCap: bytes[at + 3],
      targetMinutes: bytes[at + 4],
    };
    at += 5;
    const tick = readVarint(bytes, at);
    at = tick.next;
    if (at + TOKEN_LEN > bytes.length) throw new Error("truncated Welcome token");
    return {
      kind: "welcome",
      playerId,
      team,
      seed: seed.value,
      config,
      tick: tick.value,
      token: bytes.slice(at, at + TOKEN_LEN),
    };
  }
  if (tag === S2C_ERROR) {
    if (bytes.length < 1 + 1 + ERROR_DETAIL_LEN) {
      throw new Error("truncated Error");
    }
    const detail = new TextDecoder()
      .decode(bytes.subarray(2, 2 + ERROR_DETAIL_LEN))
      .replace(/\0.*$/, "")
      .trim();
    return { kind: "error", code: bytes[1], detail };
  }
  if (tag === S2C_PONG) {
    const t = readVarint(bytes, 1);
    return { kind: "pong", t: t.value };
  }
  return { kind: "other", tag, payload: bytes.slice(1) };
}

export function errorMessage(code: number, detail: string): string {
  const base = (() => {
    switch (code) {
      case ERROR_VERSION_MISMATCH:
        return "Your client is out of date. Reload the page to get the current version.";
      case 2:
        return "That room no longer exists.";
      case 3:
        return "That room is full or has already started.";
      case 4:
        return "That name is already taken in this room.";
      case 5:
        return "The server could not read your message.";
      case 6:
        return "You are sending messages too quickly. Wait a moment and try again.";
      case 7:
        return "The room hit an internal error. Reload or try another room.";
      default:
        return "The connection was refused.";
    }
  })();
  return detail === "" ? base : `${base}\n${detail}`;
}

export class ReconnectBackoff {
  readonly limit: number;
  private attemptCount = 0;

  constructor(limit = 6) {
    this.limit = limit;
  }

  get attempts(): number {
    return this.attemptCount;
  }

  next(random: () => number = Math.random): number | null {
    if (this.attemptCount >= this.limit) {
      return null;
    }
    const base = Math.min(8000, 500 * 2 ** this.attemptCount);
    this.attemptCount += 1;
    const jittered = base * (0.8 + 0.4 * random());
    return Math.min(8000, Math.round(jittered));
  }

  reset(): void {
    this.attemptCount = 0;
  }
}

export interface NetTimers {
  setTimeout(fn: () => void, ms: number): unknown;
  clearTimeout(id: unknown): void;
  setInterval(fn: () => void, ms: number): unknown;
  clearInterval(id: unknown): void;
}

const DEFAULT_TIMERS: NetTimers = {
  setTimeout: (fn, ms) => globalThis.setTimeout(fn, ms),
  clearTimeout: (id) => globalThis.clearTimeout(id as ReturnType<typeof setTimeout>),
  setInterval: (fn, ms) => globalThis.setInterval(fn, ms),
  clearInterval: (id) => globalThis.clearInterval(id as ReturnType<typeof setInterval>),
};

const NET_CSS = `
.net-fatal { position: fixed; inset: 0; z-index: 80; display: flex; align-items: center;
  justify-content: center; padding: 20px; background: rgba(5, 8, 12, 0.96);
  color: #f2f6fa; font-family: ui-sans-serif, system-ui, sans-serif; }
.net-fatal-panel { width: min(440px, 100%); padding: 22px; border: 1px solid rgba(255,255,255,0.16);
  border-radius: 12px; background: #0d141d; }
.net-fatal-title { margin: 0 0 10px; font-size: 16px; letter-spacing: 0.08em; text-transform: uppercase; }
.net-fatal-message { margin: 0 0 18px; color: #c4d1df; font-size: 13px; line-height: 1.5; white-space: pre-line; }
.net-fatal button { min-height: 44px; padding: 0 16px; border: 1px solid rgba(255,255,255,0.2);
  border-radius: 8px; background: #e0b36f; color: #151009; font: 650 11px ui-sans-serif, system-ui, sans-serif;
  letter-spacing: 0.08em; text-transform: uppercase; }
.net-debug { position: fixed; left: calc(env(safe-area-inset-left, 0px) + 16px);
  bottom: calc(env(safe-area-inset-bottom, 0px) + 16px); z-index: 46; display: none;
  padding: 4px 8px; border-radius: 5px; background: rgba(8,12,18,0.72);
  color: #b7c5d4; font: 600 10px ui-monospace, SFMono-Regular, Menlo, monospace; }
.net-debug.visible { display: block; }
`;

function ensureStyle(doc: Document): void {
  if (doc.getElementById(NET_STYLE_ID) !== null) {
    return;
  }
  const style = doc.createElement("style");
  style.id = NET_STYLE_ID;
  style.textContent = NET_CSS;
  doc.head.append(style);
}

/** A readable, blocking connection screen. Shared by the client and the app. */
export function showFatalScreen(
  message: string,
  parent: HTMLElement = document.body,
): HTMLElement {
  const doc = parent.ownerDocument;
  ensureStyle(doc);
  let root = doc.querySelector<HTMLElement>(".net-fatal");
  if (root === null) {
    root = doc.createElement("div");
    root.className = "net-fatal";
    const panel = doc.createElement("div");
    panel.className = "net-fatal-panel";
    const title = doc.createElement("h2");
    title.className = "net-fatal-title";
    title.textContent = "Connection problem";
    const text = doc.createElement("p");
    text.className = "net-fatal-message";
    const reload = doc.createElement("button");
    reload.type = "button";
    reload.textContent = "RELOAD";
    reload.addEventListener("click", () => doc.defaultView?.location.reload());
    panel.append(title, text, reload);
    root.append(panel);
    parent.append(root);
  }
  const text = root.querySelector(".net-fatal-message");
  if (text !== null) {
    text.textContent = message;
  }
  return root;
}

/** The small RTT readout. Shared by the client and the app test hook. */
export function showRttOverlay(ms: number, parent: HTMLElement = document.body): HTMLElement {
  const doc = parent.ownerDocument;
  ensureStyle(doc);
  let node = doc.querySelector<HTMLElement>(".net-debug");
  if (node === null) {
    node = doc.createElement("div");
    node.className = "net-debug";
    parent.append(node);
  }
  node.textContent = `PING ${Math.round(ms)} ms`;
  node.classList.add("visible");
  return node;
}

export interface NetClientOptions {
  url: string;
  roomCode: string;
  name: string;
  createTransport?: () => Transport;
  storage?: Pick<Storage, "getItem" | "setItem"> | null;
  timers?: NetTimers;
  now?: () => number;
  random?: () => number;
  overlay?: boolean;
  overlayParent?: HTMLElement;
  onWelcome?: (message: Extract<ServerMessage, { kind: "welcome" }>) => void;
  onFatal?: (message: string) => void;
  onRtt?: (ms: number) => void;
  onState?: (state: NetState) => void;
  onOther?: (message: Extract<ServerMessage, { kind: "other" }>) => void;
}

export type NetState = "idle" | "connecting" | "open" | "reconnecting" | "fatal" | "closed";

export class NetClient {
  private readonly options: NetClientOptions;
  private readonly timers: NetTimers;
  private readonly now: () => number;
  private readonly random: () => number;
  private readonly storage: Pick<Storage, "getItem" | "setItem"> | null;
  private readonly overlay: boolean;
  private readonly backoff = new ReconnectBackoff();
  private transport: Transport | null = null;
  private stateValue: NetState = "idle";
  private reconnectTimer: unknown = null;
  private pingTimer: unknown = null;
  private pingSeq = 1;
  private readonly pingSent = new Map<number, number>();
  private rttValue: number | null = null;
  private tokenValue: Uint8Array | null = null;
  private manualClose = false;

  constructor(options: NetClientOptions) {
    this.options = options;
    this.timers = options.timers ?? DEFAULT_TIMERS;
    this.now = options.now ?? (() => performance.now());
    this.random = options.random ?? Math.random;
    this.storage =
      options.storage !== undefined
        ? options.storage
        : typeof sessionStorage === "undefined"
          ? null
          : sessionStorage;
    this.overlay =
      options.overlay ?? (typeof document !== "undefined" && typeof window !== "undefined");
    const stored = this.storage?.getItem(NET_TOKEN_KEY) ?? null;
    this.tokenValue = parseToken(stored);
  }

  get state(): NetState {
    return this.stateValue;
  }

  get rttMs(): number | null {
    return this.rttValue;
  }

  get attempts(): number {
    return this.backoff.attempts;
  }

  get tokenHex(): string | null {
    return this.tokenValue === null ? null : toHex(this.tokenValue);
  }

  connect(): void {
    if (
      this.stateValue === "connecting" ||
      this.stateValue === "open" ||
      this.stateValue === "reconnecting"
    ) {
      return;
    }
    this.manualClose = false;
    this.setState("connecting");
    this.openTransport();
  }

  disconnect(): void {
    this.manualClose = true;
    this.clearTimers();
    this.transport?.close(1000, "client closed");
    this.transport = null;
    this.setState("closed");
  }

  destroy(): void {
    this.disconnect();
    if (this.overlay) {
      document.querySelector(".net-fatal")?.remove();
      document.querySelector(".net-debug")?.remove();
    }
  }

  private setState(state: NetState): void {
    this.stateValue = state;
    this.options.onState?.(state);
  }

  private openTransport(): void {
    const create = this.options.createTransport ?? (() => new WsTransport());
    const transport = create();
    this.transport = transport;
    transport.onOpen = () => this.handleOpen();
    transport.onMessage = (data) => this.handleMessage(data);
    transport.onClose = (info) => this.handleClose(info);
    transport.onError = () => {
      // The browser follows an error with close; close owns the retry.
    };
    transport.connect(this.options.url);
  }

  private handleOpen(): void {
    this.backoff.reset();
    this.setState("open");
    this.sendHello();
    this.startPing();
  }

  private sendHello(): void {
    if (this.transport === null) {
      return;
    }
    this.transport.send(
      encodeHello({
        protocol: 1,
        name: this.options.name,
        roomCode: this.options.roomCode,
        token: this.tokenValue,
      }),
    );
  }

  private startPing(): void {
    this.stopPing();
    this.pingTimer = this.timers.setInterval(() => this.sendPing(), 1000);
  }

  private stopPing(): void {
    if (this.pingTimer !== null) {
      this.timers.clearInterval(this.pingTimer);
      this.pingTimer = null;
    }
    this.pingSent.clear();
  }

  private sendPing(): void {
    if (this.stateValue !== "open" || this.transport === null) {
      return;
    }
    const t = this.pingSeq >>> 0;
    this.pingSeq = (this.pingSeq + 1) >>> 0;
    this.pingSent.set(t, this.now());
    this.transport.send(encodePing(t));
  }

  private handleMessage(data: ArrayBuffer): void {
    let message: ServerMessage;
    try {
      message = decodeS2c(data);
    } catch {
      this.fatal("The server sent a message this client cannot read.");
      return;
    }
    if (message.kind === "welcome") {
      this.tokenValue = message.token;
      this.storage?.setItem(NET_TOKEN_KEY, toHex(message.token));
      this.options.onWelcome?.(message);
      return;
    }
    if (message.kind === "error") {
      this.fatal(errorMessage(message.code, message.detail));
      return;
    }
    if (message.kind === "pong") {
      const sent = this.pingSent.get(message.t);
      if (sent !== undefined) {
        this.pingSent.delete(message.t);
        const rtt = Math.max(0, this.now() - sent);
        this.rttValue = rtt;
        if (this.overlay) {
          showRttOverlay(rtt, this.options.overlayParent ?? document.body);
        }
        this.options.onRtt?.(rtt);
      }
      return;
    }
    this.options.onOther?.(message);
  }

  private handleClose(_info: CloseInfo): void {
    if (this.manualClose || this.stateValue === "fatal") {
      return;
    }
    this.stopPing();
    this.transport = null;
    const delay = this.backoff.next(this.random);
    if (delay === null) {
      this.fatal("Could not reconnect to the room. Check your connection and reload.");
      return;
    }
    this.setState("reconnecting");
    this.reconnectTimer = this.timers.setTimeout(() => {
      this.reconnectTimer = null;
      if (this.stateValue === "reconnecting") {
        this.openTransport();
      }
    }, delay);
  }

  private fatal(message: string): void {
    if (this.stateValue === "fatal") {
      return;
    }
    this.clearTimers();
    this.transport?.close(1000, "fatal");
    this.transport = null;
    this.setState("fatal");
    if (this.overlay) {
      showFatalScreen(message, this.options.overlayParent ?? document.body);
    }
    this.options.onFatal?.(message);
  }

  private clearTimers(): void {
    this.stopPing();
    if (this.reconnectTimer !== null) {
      this.timers.clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }
}

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes, (value) => value.toString(16).padStart(2, "0")).join("");
}

function parseToken(hex: string | null): Uint8Array | null {
  if (hex === null || hex.length !== TOKEN_LEN * 2) {
    return null;
  }
  const out = new Uint8Array(TOKEN_LEN);
  for (let i = 0; i < TOKEN_LEN; i += 1) {
    const value = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
    if (!Number.isFinite(value)) {
      return null;
    }
    out[i] = value;
  }
  return out;
}
