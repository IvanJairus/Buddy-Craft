/**
 * T-040 acceptance specs: the wire vectors, the reconnect policy, the token,
 * the 1 Hz RTT probe and the protocol-mismatch screen.
 *
 * The Rust protocol encoder produced the byte vectors below; the TS codec is
 * required to round-trip exactly those frames. The client tests use a fake
 * transport and a manual clock/timer, so reconnect timing is deterministic.
 */

import { expect, test, type Page } from "@playwright/test";
import { WsTransport, type Transport, type TransportState } from "../src/game/net/transport";
import {
  ERROR_VERSION_MISMATCH,
  NetClient,
  ReconnectBackoff,
  decodeHello,
  decodeS2c,
  encodeHello,
  encodePing,
} from "../src/game/net/client";

const HELLO_VECTOR =
  "01014956414e00000000000000000000000041424331323300";
const WELCOME_VECTOR =
  "810201eeff83060102030514b960000102030405060708090a0b0c0d0e0f";
// 0x8A, code 1, and 64 bytes of detail: "out of date" plus NUL padding.
const ERROR_VECTOR =
  "8a016f7574206f662064617465" +
  "00000000000000000000000000000000" +
  "00000000000000000000000000000000" +
  "00000000000000000000000000000000" +
  "00000000000000";
const PONG_VECTOR = "8bd209";

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes, (value) => value.toString(16).padStart(2, "0")).join("");
}

function pushVarint(out: number[], value: number): void {
  let rest = value >>> 0;
  do {
    let byte = rest & 0x7f;
    rest >>>= 7;
    if (rest !== 0) byte |= 0x80;
    out.push(byte);
  } while (rest !== 0);
}

function readVarint(bytes: Uint8Array, at: number): { value: number; next: number } {
  let value = 0;
  let shift = 0;
  let offset = at;
  for (;;) {
    const byte = bytes[offset];
    offset += 1;
    value += (byte & 0x7f) * 2 ** shift;
    if ((byte & 0x80) === 0) return { value, next: offset };
    shift += 7;
  }
}

test("the TS codec matches the Rust wire vectors", () => {
  expect(bytesToHex(encodeHello({ protocol: 1, name: "IVAN", roomCode: "ABC123", token: null }))).toBe(
    HELLO_VECTOR,
  );
  expect(bytesToHex(encodePing(1234))).toBe("08d209");

  const welcome = decodeS2c(hexToBytes(WELCOME_VECTOR));
  expect(welcome.kind).toBe("welcome");
  if (welcome.kind === "welcome") {
    expect(welcome.playerId).toBe(2);
    expect(welcome.team).toBe(1);
    expect(welcome.seed).toBe(0x00c0ffee);
    expect(welcome.config).toEqual({
      mode: 1,
      teams: 2,
      perTeam: 3,
      buddyCap: 5,
      targetMinutes: 20,
    });
    expect(welcome.tick).toBe(12_345);
    expect(bytesToHex(welcome.token)).toBe("000102030405060708090a0b0c0d0e0f");
  }

  const error = decodeS2c(hexToBytes(ERROR_VECTOR));
  expect(error).toEqual({ kind: "error", code: ERROR_VERSION_MISMATCH, detail: "out of date" });
  expect(decodeS2c(hexToBytes(PONG_VECTOR))).toEqual({ kind: "pong", t: 1234 });
});

test("backoff doubles to eight seconds and stops after six", () => {
  const backoff = new ReconnectBackoff();
  const delays = Array.from({ length: 6 }, () => backoff.next(() => 0.5));
  expect(delays).toEqual([500, 1000, 2000, 4000, 8000, 8000]);
  expect(backoff.next(() => 0.5)).toBeNull();

  const low = new ReconnectBackoff();
  expect([low.next(() => 0), low.next(() => 0)]).toEqual([400, 800]);
  const high = new ReconnectBackoff();
  expect([high.next(() => 1), high.next(() => 1)]).toEqual([600, 1200]);
});

test("WsTransport sends exactly one frame per message", () => {
  class FakeSocket {
    static last: FakeSocket | null = null;
    binaryType = "";
    onopen: (() => void) | null = null;
    onmessage: ((event: MessageEvent) => void) | null = null;
    onclose: ((event: CloseEvent) => void) | null = null;
    onerror: ((event: Event) => void) | null = null;
    sent: unknown[] = [];
    closed: { code: number; reason: string } | null = null;

    constructor(public url: string) {
      FakeSocket.last = this;
    }

    send(data: unknown): void {
      this.sent.push(data);
    }

    close(code = 1000, reason = ""): void {
      this.closed = { code, reason };
    }
  }

  const transport = new WsTransport((url) => new FakeSocket(url) as unknown as WebSocket);
  let opened = 0;
  let received: ArrayBuffer | null = null;
  transport.onOpen = () => {
    opened += 1;
  };
  transport.onMessage = (data) => {
    received = data;
  };
  transport.connect("ws://room");
  const socket = FakeSocket.last!;
  expect(socket.binaryType).toBe("arraybuffer");
  socket.onopen?.();
  expect(opened).toBe(1);
  const frame = Uint8Array.from([1, 2, 3]);
  transport.send(frame);
  transport.send(frame);
  expect(socket.sent).toEqual([frame, frame]);
  const buffer = frame.buffer as ArrayBuffer;
  socket.onmessage?.({ data: buffer } as MessageEvent);
  expect(received).toBe(buffer);
  transport.close(1000, "done");
  expect(socket.closed).toEqual({ code: 1000, reason: "done" });
});

class ManualTimers {
  timeouts: Array<{ id: number; fn: () => void; ms: number }> = [];
  intervals: Array<{ id: number; fn: () => void; ms: number }> = [];
  private nextId = 1;

  setTimeout(fn: () => void, ms: number): number {
    const id = this.nextId++;
    this.timeouts.push({ id, fn, ms });
    return id;
  }

  clearTimeout(id: unknown): void {
    this.timeouts = this.timeouts.filter((entry) => entry.id !== id);
  }

  setInterval(fn: () => void, ms: number): number {
    const id = this.nextId++;
    this.intervals.push({ id, fn, ms });
    return id;
  }

  clearInterval(id: unknown): void {
    this.intervals = this.intervals.filter((entry) => entry.id !== id);
  }

  runNextTimeout(): void {
    const entry = this.timeouts.shift();
    entry?.fn();
  }

  runIntervals(): void {
    for (const entry of this.intervals) {
      entry.fn();
    }
  }
}

class MemoryStorage {
  private readonly values = new Map<string, string>();
  getItem(key: string): string | null {
    return this.values.get(key) ?? null;
  }
  setItem(key: string, value: string): void {
    this.values.set(key, value);
  }
}

class FakeTransport implements Transport {
  state: TransportState = "idle";
  onOpen: (() => void) | null = null;
  onMessage: ((data: ArrayBuffer) => void) | null = null;
  onClose: ((info: { code: number; reason: string; wasClean: boolean }) => void) | null = null;
  onError: ((event: Event) => void) | null = null;
  sent: Uint8Array[] = [];
  closeCalls = 0;

  connect(_url: string): void {
    this.state = "connecting";
  }

  send(message: Uint8Array): void {
    this.sent.push(message);
  }

  close(): void {
    this.closeCalls += 1;
    this.state = "closed";
  }

  open(): void {
    this.state = "open";
    this.onOpen?.();
  }

  message(bytes: Uint8Array): void {
    this.onMessage?.(bytes.buffer as ArrayBuffer);
  }

  serverClose(): void {
    this.state = "closed";
    this.onClose?.({ code: 1006, reason: "", wasClean: false });
  }
}

function makeClient(
  timers: ManualTimers,
  storage: MemoryStorage,
  clock: { value: number },
  transports: FakeTransport[],
  onFatal: (message: string) => void = () => {},
  onRtt: (ms: number) => void = () => {},
): NetClient {
  return new NetClient({
    url: "ws://room",
    roomCode: "ABC123",
    name: "IVAN",
    createTransport: () => {
      const transport = new FakeTransport();
      transports.push(transport);
      return transport;
    },
    storage,
    timers,
    now: () => clock.value,
    random: () => 0.5,
    overlay: false,
    onFatal,
    onRtt,
  });
}

test("the reconnect presents the welcome token and measures RTT", () => {
  const timers = new ManualTimers();
  const storage = new MemoryStorage();
  const clock = { value: 1000 };
  const transports: FakeTransport[] = [];
  const rtts: number[] = [];
  const client = makeClient(timers, storage, clock, transports, () => {}, (ms) => rtts.push(ms));

  client.connect();
  const first = transports[0];
  first.open();
  expect(decodeHello(first.sent[0]).token).toBeNull();
  first.message(hexToBytes(WELCOME_VECTOR));
  expect(client.tokenHex).toBe("000102030405060708090a0b0c0d0e0f");
  expect(storage.getItem("buddycraft.netToken")).toBe(client.tokenHex);

  first.serverClose();
  expect(client.state).toBe("reconnecting");
  expect(timers.timeouts[0]?.ms).toBe(500);
  timers.runNextTimeout();
  const second = transports[1];
  second.open();
  const hello = decodeHello(second.sent[0]);
  expect(bytesToHex(hello.token!)).toBe("000102030405060708090a0b0c0d0e0f");

  timers.runIntervals();
  const ping = second.sent[second.sent.length - 1];
  expect(ping[0]).toBe(0x08);
  const t = readVarint(ping, 1).value;
  clock.value += 40;
  const pong: number[] = [0x8b];
  pushVarint(pong, t);
  second.message(Uint8Array.from(pong));
  expect(client.rttMs).toBe(40);
  expect(rtts).toEqual([40]);
  client.destroy();
});

test("a protocol mismatch is fatal and does not schedule a reconnect", () => {
  const timers = new ManualTimers();
  const storage = new MemoryStorage();
  const clock = { value: 0 };
  const transports: FakeTransport[] = [];
  const fatals: string[] = [];
  const client = makeClient(timers, storage, clock, transports, (message) => fatals.push(message));

  client.connect();
  transports[0].open();
  transports[0].message(hexToBytes(ERROR_VECTOR));
  expect(client.state).toBe("fatal");
  expect(fatals[0]).toContain("out of date");
  expect(timers.timeouts).toHaveLength(0);
  expect(transports[0].closeCalls).toBe(1);
  client.destroy();
});

test("six failed reconnects stop with a readable failure", () => {
  const timers = new ManualTimers();
  const storage = new MemoryStorage();
  const clock = { value: 0 };
  const transports: FakeTransport[] = [];
  const fatals: string[] = [];
  const client = makeClient(timers, storage, clock, transports, (message) => fatals.push(message));

  client.connect();
  transports[0].serverClose();
  for (let attempt = 0; attempt < 6; attempt += 1) {
    expect(timers.timeouts.length).toBeGreaterThan(0);
    timers.runNextTimeout();
    const latest = transports[transports.length - 1];
    latest.serverClose();
  }
  expect(client.state).toBe("fatal");
  expect(fatals[0]).toContain("Could not reconnect");
  expect(transports).toHaveLength(7);
  client.destroy();
});

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    netFatal(code: number, detail?: string): string;
    netRtt(ms: number): void;
  };
}

async function boot(page: Page): Promise<void> {
  await page.goto("/?quality=high&scene=clear&screen=match");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
}

test("protocol_mismatch_shows_message", async ({ page }) => {
  await boot(page);
  const message = await page.evaluate(
    (code) =>
      (window as unknown as TestWindow).__cbTest!.netFatal(
        code,
        "server protocol 2, client protocol 1",
      ),
    ERROR_VERSION_MISMATCH,
  );
  expect(message).toContain("out of date");
  const overlay = page.locator(".net-fatal");
  await expect(overlay).toBeVisible();
  expect(await overlay.textContent()).toContain("out of date");
  expect(await overlay.locator("button").textContent()).toBe("RELOAD");

  await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.netRtt(42));
  const debug = page.locator(".net-debug");
  await expect(debug).toBeVisible();
  expect(await debug.textContent()).toBe("PING 42 ms");
});
