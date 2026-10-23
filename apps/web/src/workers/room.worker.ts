/// <reference lib="webworker" />
/**
 * Offline room worker (task T-048).
 *
 * The worker owns the wasm offline session and speaks the same C2S/S2C bytes as
 * the WebSocket host. The main thread never imports `room` and never gets a
 * second rule implementation: it sends input bytes and receives snapshot bytes.
 */

import wasmUrl from "../wasm/client_wasm.wasm?url";
import { loadClientWasm, type ClientWasmExports } from "../wasm/loader";

interface WorkerInit {
  type: "init";
  seed: number;
  mode: number;
  teams: number;
}

interface WorkerMessage {
  type: "message";
  bytes: Uint8Array;
}

interface WorkerAdvance {
  type: "advance";
  ticks: number;
  post?: boolean;
}

interface WorkerClose {
  type: "close";
}

interface WorkerPause {
  type: "pause" | "resume";
}

type Incoming = WorkerInit | WorkerMessage | WorkerAdvance | WorkerClose | WorkerPause;

interface Scope {
  postMessage(message: unknown, transfer?: Transferable[]): void;
  setInterval(fn: () => void, ms: number): number;
  clearInterval(id: number): void;
  onmessage: ((event: MessageEvent<Incoming>) => void) | null;
}

const scope = self as unknown as Scope;
const POLL_BYTES = 4096;
let wasm: ClientWasmExports | null = null;
let timer = 0;
let outPtr = 0;
let inputPtr = 0;
let ended = false;

scope.onmessage = (event: MessageEvent<Incoming>): void => {
  const message = event.data;
  if (message.type === "init") {
    void init(message);
    return;
  }
  if (message.type === "close") {
    stop();
    self.close();
    return;
  }
  if (message.type === "pause") {
    if (timer !== 0) {
      scope.clearInterval(timer);
      timer = 0;
    }
    return;
  }
  if (message.type === "resume") {
    if (wasm !== null && !ended && timer === 0) {
      timer = scope.setInterval(tick, 50);
    }
    return;
  }
  if (wasm === null) {
    return;
  }
  if (message.type === "message") {
    const bytes = message.bytes;
    if (bytes.byteLength > POLL_BYTES) return;
    new Uint8Array(wasm.memory.buffer, inputPtr, bytes.byteLength).set(bytes);
    wasm.cb_offline_message(inputPtr, bytes.byteLength);
    pump(true);
    return;
  }
  if (message.type === "advance") {
    let final = 0;
    for (let i = 0; i < message.ticks && !ended; i += 1) {
      wasm.cb_offline_tick();
      let written = 0;
      while ((written = wasm.cb_offline_poll(outPtr, POLL_BYTES)) > 0) {
        const view = new Uint8Array(wasm.memory.buffer, outPtr, written);
        if (view[0] === 0x89) ended = true;
        final = written;
        if (message.post) {
          const copy = view.slice();
          scope.postMessage({ type: "frame", bytes: copy.buffer }, [copy.buffer]);
        }
      }
    }
    scope.postMessage({
      type: "advanced",
      ended,
      hash: wasm.cb_offline_state_hash() >>> 0,
      final,
    });
  }
};

async function init(message: WorkerInit): Promise<void> {
  try {
    const exports = await loadClientWasm(wasmUrl);
    wasm = exports;
    wasm.cb_init(message.seed, 0, 0, 0);
    outPtr = wasm.cb_mem_alloc(POLL_BYTES);
    inputPtr = wasm.cb_mem_alloc(POLL_BYTES);
    if (outPtr === 0 || inputPtr === 0 || wasm.cb_offline_init(message.seed, message.mode, message.teams) === 0) {
      scope.postMessage({ type: "error", detail: "offline wasm init failed" });
      return;
    }
    scope.postMessage({ type: "open" });
    pump(true);
    timer = scope.setInterval(tick, 50);
  } catch (error) {
    scope.postMessage({
      type: "error",
      detail: error instanceof Error ? error.message : String(error),
    });
  }
}

function tick(): void {
  if (wasm === null || ended) return;
  wasm.cb_offline_tick();
  pump(true);
}

function pump(post: boolean): void {
  if (wasm === null) return;
  for (;;) {
    const written = wasm.cb_offline_poll(outPtr, POLL_BYTES);
    if (written === 0) return;
    const copy = new Uint8Array(wasm.memory.buffer, outPtr, written).slice();
    if (copy[0] === 0x89) ended = true;
    if (post) scope.postMessage({ type: "frame", bytes: copy.buffer }, [copy.buffer]);
  }
}

function stop(): void {
  if (timer !== 0) {
    scope.clearInterval(timer);
    timer = 0;
  }
}
