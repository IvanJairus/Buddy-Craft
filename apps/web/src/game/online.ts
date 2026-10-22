/**
 * Online room session (task T-054).
 *
 * Wraps the same `NetClient` the offline Worker path uses, but over a real
 * WebSocket. The caller sees one `onFrame` stream of S2C bytes, so snapshot,
 * chunk-diff and edit handling stay identical between offline and online play.
 */

import { NetClient, encodeEdit, encodeInput, snapshotSeq } from "./net/client";
import type { NetState, ServerMessage } from "./net/client";

export interface OnlineOptions {
  server: string;
  roomCode: string;
  name: string;
  onFrame: (bytes: Uint8Array) => void;
  onWelcome: (message: Extract<ServerMessage, { kind: "welcome" }>) => void;
  onEnd: (bytes: Uint8Array) => void;
  onState?: (state: NetState) => void;
  onFatal?: (message: string) => void;
}

/** `/ws?room=CODE` for a base like `ws://127.0.0.1:8080`. */
export function roomUrl(server: string, roomCode: string): string {
  const url = new URL(server);
  url.pathname = "/ws";
  url.search = `?room=${encodeURIComponent(roomCode)}`;
  return url.toString();
}

export class OnlineSession {
  readonly client: NetClient;
  readonly code: string;
  private inputSeq = 1;
  private editSeq = 1;
  private firstTick = 1;
  private acked = 0;
  private snapshotFrames = 0;
  private chunkDiffs = 0;

  constructor(options: OnlineOptions) {
    this.code = options.roomCode;
    this.client = new NetClient({
      url: roomUrl(options.server, options.roomCode),
      roomCode: options.roomCode,
      name: options.name,
      onWelcome: (message) => options.onWelcome(message),
      onState: (state) => options.onState?.(state),
      onFatal: (message) => options.onFatal?.(message),
      onRaw: (bytes) => {
        if (bytes[0] === 0x84) {
          const seq = snapshotSeq(bytes);
          if (seq !== null) this.acked = seq;
          this.snapshotFrames += 1;
        } else if (bytes[0] === 0x87) {
          this.chunkDiffs += 1;
        }
        options.onFrame(bytes);
        if (bytes[0] === 0x89) options.onEnd(bytes);
      },
    });
    this.client.connect();
  }

  get state(): NetState {
    return this.client.state;
  }

  get rttMs(): number | null {
    return this.client.rttMs;
  }

  get frames(): { snapshots: number; chunkDiffs: number } {
    return { snapshots: this.snapshotFrames, chunkDiffs: this.chunkDiffs };
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

  stop(): void {
    this.client.destroy();
  }
}
