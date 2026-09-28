import type { MemoryDConfig } from "./types";

const MAX_TIMEOUT_MS = 10_000;
const MAX_TOKENS = 16_000;
const MAX_RESPONSE_BYTES = 4 * 1024 * 1024;

export const DEFAULT_CONFIG: MemoryDConfig = {
  baseUrl: "http://127.0.0.1:8787",
  profile: "",
  workspace: "",
  autoRecall: true,
  autoObserve: false,
  recallTimeoutMs: 500,
  maxTokens: 1_200,
  maxResponseBytes: MAX_RESPONSE_BYTES,
};

export class MemoryDConfigError extends Error {
  readonly code: "invalid_endpoint" | "missing_scope" | "invalid_value" | "writeback_unavailable";

  constructor(code: MemoryDConfigError["code"], message: string) {
    super(message);
    this.name = "MemoryDConfigError";
    this.code = code;
  }
}

function loopbackEndpoint(value: unknown): string {
  if (typeof value !== "string" || value.trim() === "") {
    throw new MemoryDConfigError("invalid_endpoint", "MemoryD baseUrl is required");
  }
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new MemoryDConfigError("invalid_endpoint", "MemoryD baseUrl is not a URL");
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new MemoryDConfigError("invalid_endpoint", "MemoryD baseUrl must use HTTP(S)");
  }
  if (url.username || url.password || url.search || url.hash || url.pathname !== "/" && url.pathname !== "") {
    throw new MemoryDConfigError("invalid_endpoint", "MemoryD baseUrl must be a loopback origin without credentials or paths");
  }
  const host = url.hostname.toLowerCase().replace(/^\[(.*)\]$/, "$1");
  if (host !== "localhost" && host !== "127.0.0.1" && host !== "::1") {
    throw new MemoryDConfigError("invalid_endpoint", "MemoryD baseUrl must use a loopback host");
  }
  url.pathname = "/";
  url.search = "";
  url.hash = "";
  return url.toString().replace(/\/$/, "");
}

function boundedNumber(value: unknown, fallback: number, min: number, max: number, name: string): number {
  if (value === undefined) return fallback;
  const number = typeof value === "number" ? value : Number(value);
  if (!Number.isFinite(number) || number < min || number > max) {
    throw new MemoryDConfigError("invalid_value", `${name} is outside its supported bounds`);
  }
  return Math.floor(number);
}

function requiredScope(value: unknown, name: "profile" | "workspace"): string {
  if (typeof value !== "string" || value.trim() === "") {
    throw new MemoryDConfigError("missing_scope", `MemoryD ${name} must be configured explicitly`);
  }
  return value.trim();
}

type ConfigInput = Omit<Partial<MemoryDConfig>, "autoObserve"> & { autoObserve?: unknown };
export function loadConfig(input: ConfigInput = {}): MemoryDConfig {
  if (input.autoObserve === true) {
    throw new MemoryDConfigError(
      "writeback_unavailable",
      "Automatic OMP observation is disabled until the MemoryD host-observation receipt API is available",
    );
  }
  return {
    baseUrl: loopbackEndpoint(input.baseUrl ?? DEFAULT_CONFIG.baseUrl),
    profile: requiredScope(input.profile, "profile"),
    workspace: requiredScope(input.workspace, "workspace"),
    autoRecall: input.autoRecall !== false,
    autoObserve: false,
    recallTimeoutMs: boundedNumber(input.recallTimeoutMs, DEFAULT_CONFIG.recallTimeoutMs, 25, MAX_TIMEOUT_MS, "recallTimeoutMs"),
    maxTokens: boundedNumber(input.maxTokens, DEFAULT_CONFIG.maxTokens, 1, MAX_TOKENS, "maxTokens"),
    maxResponseBytes: boundedNumber(input.maxResponseBytes, MAX_RESPONSE_BYTES, 1024, MAX_RESPONSE_BYTES, "maxResponseBytes"),
  };
}

export function tryLoadConfig(input: ConfigInput = {}):
  | { config: MemoryDConfig; error?: undefined }
  | { config?: undefined; error: MemoryDConfigError } {
  try {
    return { config: loadConfig(input) };
  } catch (error) {
    if (error instanceof MemoryDConfigError) return { error };
    throw error;
  }
}
