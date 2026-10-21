/**
 * WebSocket transport (task T-040).
 *
 * The interface exists so tests and offline mode can drive the net client with
 * something other than a browser socket. `WsTransport.send` issues exactly one
 * `WebSocket.send` per protocol message, which is the frame contract in §7:
 * one message per binary frame, no batching.
 */

export type TransportState = "idle" | "connecting" | "open" | "closed";

export interface CloseInfo {
  code: number;
  reason: string;
  wasClean: boolean;
}

export interface Transport {
  readonly state: TransportState;
  connect(url: string): void;
  send(message: Uint8Array): void;
  close(code?: number, reason?: string): void;
  onOpen: (() => void) | null;
  onMessage: ((data: ArrayBuffer) => void) | null;
  onClose: ((info: CloseInfo) => void) | null;
  onError: ((event: Event) => void) | null;
}

export type WebSocketFactory = (url: string) => WebSocket;

export class WsTransport implements Transport {
  private socket: WebSocket | null = null;
  private stateValue: TransportState = "idle";
  private readonly factory: WebSocketFactory;

  onOpen: (() => void) | null = null;
  onMessage: ((data: ArrayBuffer) => void) | null = null;
  onClose: ((info: CloseInfo) => void) | null = null;
  onError: ((event: Event) => void) | null = null;

  constructor(factory: WebSocketFactory = (url) => new WebSocket(url)) {
    this.factory = factory;
  }

  get state(): TransportState {
    return this.stateValue;
  }

  connect(url: string): void {
    if (this.stateValue === "connecting" || this.stateValue === "open") {
      return;
    }
    this.stateValue = "connecting";
    const socket = this.factory(url);
    socket.binaryType = "arraybuffer";
    this.socket = socket;
    socket.onopen = () => {
      this.stateValue = "open";
      this.onOpen?.();
    };
    socket.onmessage = (event: MessageEvent) => {
      if (event.data instanceof ArrayBuffer) {
        this.onMessage?.(event.data);
      }
    };
    socket.onclose = (event: CloseEvent) => {
      this.stateValue = "closed";
      this.socket = null;
      this.onClose?.({
        code: event.code,
        reason: event.reason,
        wasClean: event.wasClean,
      });
    };
    socket.onerror = (event: Event) => {
      this.onError?.(event);
    };
  }

  send(message: Uint8Array): void {
    if (this.socket === null || this.stateValue !== "open") {
      throw new Error("transport is not open");
    }
    this.socket.send(message);
  }

  close(code = 1000, reason = ""): void {
    if (this.socket !== null) {
      this.socket.close(code, reason);
    }
    this.stateValue = "closed";
    this.socket = null;
  }
}
