import { isRecord } from "./guards";
import type { MemoryDConfig } from "./types";

export type MemoryDFailureKind =
  | "cancelled"
  | "timeout"
  | "unavailable"
  | "protocol-mismatch"
  | "oversized"
  | "scope-denied"
  | "http";

export class MemoryDClientError extends Error {
  readonly kind: MemoryDFailureKind;
  readonly status?: number;

  constructor(kind: MemoryDFailureKind, status?: number) {
    super(`MemoryD ${kind}`);
    this.name = "MemoryDClientError";
    this.kind = kind;
    this.status = status;
  }
}

export interface RecallFact {
  id?: string;
  content?: unknown;
  updated_at?: unknown;
  stale?: unknown;
  policy?: {
    freshness?: { stale?: unknown; age_days?: unknown };
    provenance?: Record<string, unknown>;
    admission?: Record<string, unknown>;
  };
}

export interface RecallData {
  summary?: unknown;
  facts?: unknown;
  checkpoints?: unknown;
  citations?: unknown;
  withheld?: unknown;
  truncated?: unknown;
  authority?: unknown;
  policy?: unknown;
  pack?: unknown;
}

export interface SearchData {
  matches?: unknown;
  next_cursor?: unknown;
}

export interface StatusData {
  [key: string]: unknown;
}

interface Envelope<T> {
  ok?: unknown;
  data?: unknown;
  provider?: unknown;
}


function classifyAbort(signal: AbortSignal, timedOut: boolean): MemoryDClientError {
  return new MemoryDClientError(signal.aborted && !timedOut ? "cancelled" : "timeout");
}

export class MemoryDClient {
  readonly config: MemoryDConfig;

  constructor(config: MemoryDConfig) {
    this.config = config;
  }

  async health(signal?: AbortSignal): Promise<boolean> {
    try {
      await this.request("/healthz", { method: "GET", signal, timeoutMs: Math.min(this.config.recallTimeoutMs, 500) });
      return true;
    } catch {
      return false;
    }
  }

  async status(signal?: AbortSignal): Promise<StatusData> {
    return this.requestData<StatusData>("/v1/status", { method: "GET", signal });
  }

  async recall(
    request: {
      profile: string;
      workspace: string;
      query: string;
      sessionId?: string;
      repoId?: string;
      branch?: string;
      commit?: string;
      files?: readonly string[];
      maxTokens: number;
      signal?: AbortSignal;
    },
  ): Promise<RecallData> {
    return this.requestData<RecallData>("/v1/recall", {
      method: "POST",
      signal: request.signal,
      body: {
        profile: request.profile,
        workspace: request.workspace,
        query: request.query,
        session: request.sessionId ? { id: request.sessionId, source: "omp" } : undefined,
        repo: request.repoId ? { repo_id: request.repoId, branch: request.branch, commit: request.commit } : undefined,
        files: request.files?.slice(0, 32),
        max_tokens: request.maxTokens,
        pack_mode: "active_task",
        metadata: { source_kind: "omp_native_recall" },
      },
    });
  }

  async search(
    request: {
      profile: string;
      workspace: string;
      query: string;
      repoId?: string;
      limit?: number;
      signal?: AbortSignal;
    },
  ): Promise<SearchData> {
    return this.requestData<SearchData>("/v1/search", {
      method: "POST",
      signal: request.signal,
      body: {
        profile: request.profile,
        workspace: request.workspace,
        query: request.query,
        repo: request.repoId ? { repo_id: request.repoId } : undefined,
        limit: Math.max(1, Math.min(100, Math.floor(request.limit ?? 20))),
      },
    });
  }

  async explicitSave(request: {
    profile: string;
    workspace: string;
    content: string;
    context?: string;
    source?: string;
    sessionId?: string;
    signal?: AbortSignal;
  }): Promise<{ created?: unknown; record_ids?: unknown; rejected?: unknown }> {
    return this.requestData("/v1/conclusions", {
      method: "POST",
      signal: request.signal,
      body: {
        profile: request.profile,
        workspace: request.workspace,
        target: "assistant",
        conclusions: [request.content],
        metadata: {
          source_kind: "omp_explicit_save",
          source: request.source,
          context: request.context,
          session_id: request.sessionId,
          repo_identity: { status: "unsupported", reason: "OMP operation context has no sanitized remote identity" },
        },
      },
    });
  }

  private async requestData<T>(path: string, options: RequestOptions): Promise<T> {
    const data = await this.request(path, options);
    if (!isRecord(data) || data.ok !== true || !isRecord(data.data)) {
      throw new MemoryDClientError("protocol-mismatch");
    }
    return data.data as T;
  }

  private async request(path: string, options: RequestOptions): Promise<Envelope<unknown>> {
    const timeoutMs = options.timeoutMs ?? this.config.recallTimeoutMs;
    const controller = new AbortController();
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      controller.abort();
    }, timeoutMs);
    const abort = () => controller.abort();
    if (options.signal?.aborted) {
      clearTimeout(timer);
      throw new MemoryDClientError("cancelled");
    }
    options.signal?.addEventListener("abort", abort, { once: true });
    try {
      const response = await fetch(`${this.config.baseUrl}${path}`, {
        method: options.method,
        headers: options.body ? { "content-type": "application/json", accept: "application/json" } : { accept: "application/json" },
        body: options.body ? JSON.stringify(options.body) : undefined,
        redirect: "error",
        signal: controller.signal,
      });
      if (!response.ok) {
        if (response.status === 401 || response.status === 403 || response.status === 404) {
          throw new MemoryDClientError("scope-denied", response.status);
        }
        throw new MemoryDClientError("http", response.status);
      }
      const contentType = response.headers.get("content-type") ?? "";
      if (path !== "/healthz" && !contentType.toLowerCase().includes("application/json")) {
        throw new MemoryDClientError("protocol-mismatch", response.status);
      }
      const bytes = await readBoundedBody(response, this.config.maxResponseBytes, controller.signal, options.signal, timedOut);
      if (path === "/healthz") return { ok: true };
      let parsed: unknown;
      try {
        parsed = JSON.parse(new TextDecoder().decode(bytes));
      } catch {
        throw new MemoryDClientError("protocol-mismatch", response.status);
      }
      if (!isRecord(parsed)) throw new MemoryDClientError("protocol-mismatch", response.status);
      return parsed as Envelope<unknown>;
    } catch (error) {
      if (error instanceof MemoryDClientError) throw error;
      if (controller.signal.aborted) throw classifyAbort(options.signal ?? controller.signal, timedOut);
      throw new MemoryDClientError("unavailable");
    } finally {
      clearTimeout(timer);
      options.signal?.removeEventListener("abort", abort);
    }
  }
}

interface RequestOptions {
  method: "GET" | "POST";
  body?: Record<string, unknown>;
  signal?: AbortSignal;
  timeoutMs?: number;
}

async function readBoundedBody(
  response: Response,
  limit: number,
  signal: AbortSignal,
  userSignal: AbortSignal | undefined,
  timedOut: boolean,
): Promise<Uint8Array> {
  if (response.body === null) {
    const raw = new Uint8Array(await response.arrayBuffer());
    if (raw.byteLength > limit) throw new MemoryDClientError("oversized");
    return raw;
  }
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    while (true) {
      const next = await reader.read();
      if (next.done) break;
      size += next.value.byteLength;
      if (size > limit) {
        await reader.cancel();
        throw new MemoryDClientError("oversized");
      }
      chunks.push(next.value);
    }
  } catch (error) {
    if (error instanceof MemoryDClientError) throw error;
    if (signal.aborted) throw classifyAbort(userSignal ?? signal, timedOut);
    throw new MemoryDClientError("unavailable");
  } finally {
    reader.releaseLock();
  }
  const result = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) {
    result.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return result;
}
