/**
 * Offline session (task T-048).
 *
 * A real Web Worker runs the room wasm. The main thread sees a `Transport`, the
 * same `NetClient`, and the same C2S/S2C bytes as the WebSocket path. Starting
 * offline is a transport substitution, not a second game loop.
 */

import { NetClient, encodeEdit, encodeInput } from "./net/client";
import type { Transport, TransportState, CloseInfo } from "./net/transport";

export interface OfflineOptions {
  seed: number;
  mode: number;
  teams: number;
  onFrame: (bytes: Uint8Array) => void;
  onEnd: () => void;
  onWelcome?: () => void;
  onAdvanced?: (ended: boolean, hash: number) => void;
}

type WorkerEvent =
  | { type: "open" }
  | { type: "frame"; bytes: ArrayBuffer }
  | { type: "error"; detail: string }
  | { type: "advanced"; ended: boolean; hash: number; final: number };

class WorkerTransport implements Transport {
  state: TransportState = "idle";
  onOpen: (() => void) | null = null;
  onMessage: ((data: ArrayBuffer) => void) | null = null;
  onClose: ((info: CloseInfo) => void) | null = null;
  onError: ((event: Event) => void) | null = null;
  onAdvanced: ((ended: boolean, hash: number) => void) | null = null;

  private worker: Worker | null = null;
  private readonly seed: number;
  private readonly mode: number;
  private readonly teams: number;

  constructor(seed: number, mode: number, teams: number) {
    this.seed = seed;
    this.mode = mode;
    this.teams = teams;
  }

  connect(_url: string): void {
    if (this.state !== "idle" && this.state !== "closed") return;
    this.state = "connecting";
    const worker = new Worker(new URL("../workers/room.worker.ts", import.meta.url), {
      type: "module",
    });
    this.worker = worker;
    worker.onmessage = (event: MessageEvent<WorkerEvent>) => {
      const message = event.data;
      if (message.type === "open") {
        this.state = "open";
        this.onOpen?.();
        return;
      }
      if (message.type === "frame") {
        this.onMessage?.(message.bytes);
        return;
      }
      if (message.type === "advanced") {
        this.onAdvanced?.(message.ended, message.hash);
        return;
      }
      this.state = "closed";
      this.onError?.(new Event("error"));
      this.onClose?.({ code: 1011, reason: message.detail, wasClean: false });
    };
    worker.postMessage({ type: "init", seed: this.seed, mode: this.mode, teams: this.teams });
  }

  send(message: Uint8Array): void {
    if (this.worker === null || this.state !== "open") {
      throw new Error("offline transport is not open");
    }
    this.worker.postMessage({ type: "message", bytes: message });
  }

  advance(ticks: number): void {
    this.worker?.postMessage({ type: "advance", ticks });
  }

  close(_code = 1000, _reason = ""): void {
    this.state = "closed";
    this.worker?.postMessage({ type: "close" });
    this.worker?.terminate();
    this.worker = null;
  }
}

export class OfflineSession {
  readonly client: NetClient;
  private readonly transport: WorkerTransport;
  private inputSeq = 1;
  private editSeq = 1;
  private firstTick = 1;
  private acked = 0;
  private lastHash = 0;
  private lastEnded = false;

  constructor(options: OfflineOptions) {
    this.transport = new WorkerTransport(options.seed, options.mode, options.teams);
    this.transport.onAdvanced = (ended, hash) => {
      this.lastEnded = ended;
      this.lastHash = hash;
      options.onAdvanced?.(ended, hash);
    };
    this.client = new NetClient({
      url: "offline://room",
      roomCode: "OFFLINE",
      name: "PLAYER",
      createTransport: () => this.transport,
      storage: null,
      overlay: false,
      onWelcome: () => options.onWelcome?.(),
      onRaw: (bytes) => {
        if (bytes[0] === 0x84) {
          const parsed = snapshotSeq(bytes);
          if (parsed !== null) this.acked = parsed;
        }
        options.onFrame(bytes);
        if (bytes[0] === 0x89) options.onEnd();
      },
    });
    this.client.connect();
  }

  get state(): TransportState {
    return this.transport.state;
  }

  get hash(): number {
    return this.lastHash;
  }

  get ended(): boolean {
    return this.lastEnded;
  }

  sendInput(frame: Uint8Array): void {
    if (this.state !== "open") return;
    const wire = {
      axes: frame[3],
      yaw: frame[1],
      pitch: frame[2],
      buttons: frame[0],
    };
    this.client.sendRaw(
      encodeInput(this.inputSeq, this.acked, this.firstTick, [wire, wire, wire]),
    );
    this.inputSeq = (this.inputSeq + 1) & 0xffff;
    this.firstTick += 1;
  }

  sendEdit(x: number, y: number, z: number, block: number, action: number): number {
    if (this.state !== "open") return 0;
    const seq = this.editSeq;
    this.editSeq = (this.editSeq + 1) & 0xffff;
    this.client.sendRaw(encodeEdit({ seq, x, y, z, block, action }));
    return seq;
  }

  advance(ticks: number): void {
    this.transport.advance(ticks);
  }

  stop(): void {
    this.client.destroy();
  }
}

function snapshotSeq(bytes: Uint8Array): number | null {
  // tag, tick varint, seq varint
  let at = 1;
  const skip = (): number | null => {
    let value = 0;
    let shift = 0;
    for (;;) {
      if (at >= bytes.length || shift > 35) return null;
      const byte = bytes[at];
      at += 1;
      value += (byte & 0x7f) * 2 ** shift;
      if ((byte & 0x80) === 0) return value >>> 0;
      shift += 7;
    }
  };
  if (skip() === null) return null;
  const seq = skip();
  return seq === null ? null : seq;
}
