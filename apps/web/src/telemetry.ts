/**
 * Client crash reporting (task T-056, §13).
 *
 * The endpoint is configured at build time through `VITE_TELEMETRY_URL`; a
 * real deployment points it at GlitchTip/Sentry's ingest URL. With no endpoint
 * the reporter still installs its listeners and keeps the last report in
 * memory for the test hook, but sends nothing.
 */

export interface CrashReport {
  kind: "error" | "unhandledrejection" | "wasm";
  message: string;
  stack: string;
  atMs: number;
  build: string;
}

export function buildCrashReport(
  kind: CrashReport["kind"],
  error: unknown,
  atMs: number,
  build: string,
): CrashReport {
  if (error instanceof Error) {
    return {
      kind,
      message: error.message,
      stack: error.stack ?? "",
      atMs,
      build,
    };
  }
  return {
    kind,
    message: String(error),
    stack: "",
    atMs,
    build,
  };
}

export class CrashReporter {
  private readonly endpoint: string | null;
  private last: CrashReport | null = null;

  constructor(
    endpoint: string | null,
    private readonly build = "dev",
    private readonly transport: (endpoint: string, body: string) => void = defaultTransport,
  ) {
    this.endpoint = endpoint === null || endpoint === "" ? null : endpoint;
  }

  get lastReport(): CrashReport | null {
    return this.last;
  }

  install(target: Window = window): () => void {
    const onError = (event: ErrorEvent): void => {
      this.capture("error", event.error ?? event.message);
    };
    const onRejection = (event: PromiseRejectionEvent): void => {
      this.capture("unhandledrejection", event.reason);
    };
    target.addEventListener("error", onError);
    target.addEventListener("unhandledrejection", onRejection);
    return () => {
      target.removeEventListener("error", onError);
      target.removeEventListener("unhandledrejection", onRejection);
    };
  }

  capture(kind: CrashReport["kind"], error: unknown): CrashReport {
    const report = buildCrashReport(kind, error, Date.now(), this.build);
    this.last = report;
    if (this.endpoint !== null) {
      this.transport(this.endpoint, JSON.stringify(report));
    }
    return report;
  }
}

function defaultTransport(endpoint: string, body: string): void {
  if (typeof navigator !== "undefined" && typeof navigator.sendBeacon === "function") {
    const blob = new Blob([body], { type: "application/json" });
    if (navigator.sendBeacon(endpoint, blob)) return;
  }
  void fetch(endpoint, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body,
    keepalive: true,
  }).catch(() => {
    // A crash reporter that throws is worse than a dropped report.
  });
}
