import { describe, expect, test } from "bun:test";
import { createMemoryDBackend } from "../src/backend";
import { MemoryDClient, MemoryDClientError } from "../src/client";
import { loadConfig, MemoryDConfigError } from "../src/config";
import { formatRecall } from "../src/format";
import { registration, registerMemoryD, safeConfigFromSettings } from "../src/index";
import type { MemoryDConfig } from "../src/types";

const config: MemoryDConfig = {
  baseUrl: "http://127.0.0.1:8787",
  profile: "personal",
  workspace: "josh-personal",
  autoRecall: true,
  autoObserve: false,
  recallTimeoutMs: 500,
  maxTokens: 1200,
  maxResponseBytes: 4 * 1024 * 1024,
};

function envelope(data: unknown): Response {
  return new Response(JSON.stringify({ ok: true, data }), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

describe("configuration and registration", () => {
  test("requires explicit scope and accepts loopback only", () => {
    expect(() => loadConfig({ profile: "", workspace: "work" })).toThrow(MemoryDConfigError);
    expect(() => loadConfig({ profile: "personal", workspace: "work", baseUrl: "https://example.test" })).toThrow(MemoryDConfigError);
    expect(loadConfig({ profile: "personal", workspace: "work" }).recallTimeoutMs).toBe(500);
  });

  test("automatic observation cannot be enabled before #233", () => {
    expect(() => loadConfig({ profile: "personal", workspace: "work", autoObserve: true })).toThrow("receipt API");
  });

  test("uses the native registration entrypoint", () => {
    const calls: unknown[] = [];
    registerMemoryD({ registerMemoryBackend: backend => calls.push(backend) });
    expect(calls).toHaveLength(1);
    expect(registration.capabilities.automaticObservation).toBe(false);
    expect((calls[0] as typeof registration).id).toBe("codex-memoryd");
  });

  test("invalid settings produce an inactive backend instead of broadening scope", () => {
    const result = safeConfigFromSettings({ "codexMemoryd.profile": "personal" });
    expect(result.config).toBeUndefined();
    expect(result.error?.code).toBe("missing_scope");
  });
});

describe("recall formatting", () => {
  test("preserves authority, freshness, provenance and withheld state within budget", () => {
    const rendered = formatRecall({
      authority: "recall_not_authority",
      facts: [{
        id: "mem_1",
        content: "Use the checked-in fixture.",
        stale: true,
        policy: {
          freshness: { stale: true, age_days: 3 },
          provenance: { source_kind: "host_visible_context", trust_level: "weak" },
        },
      }],
      withheld: [{ reason: "quarantined", count: 1 }],
    }, 1200);
    expect(rendered.context).toContain("recall_not_authority");
    expect(rendered.context).toContain("freshness: stale");
    expect(rendered.context).toContain("source_kind: host_visible_context");
    expect(rendered.context).toContain("withheld: 1");
  });

  test("does not render a fact larger than the total budget", () => {
    const rendered = formatRecall({ facts: [{ content: "x".repeat(500) }] }, 10);
    expect(rendered.count).toBe(0);
    expect(rendered.truncated).toBe(true);
  });
  test("ignores malformed recall entries", () => {
    const rendered = formatRecall({ facts: [null, "not-an-object", 42, { content: "safe" }] }, 1200);
    expect(rendered.count).toBe(1);
    expect(rendered.context).toContain("safe");
  });
});

describe("transport and lifecycle", () => {
  test("fails open on non-JSON and never exposes response text", async () => {
    const original = globalThis.fetch;
    globalThis.fetch = async () => new Response("private server exception", { status: 500 });
    try {
      const client = new MemoryDClient(config);
      await expect(client.status()).rejects.toMatchObject({ kind: "http", status: 500 });
      await expect(client.recall({ profile: "personal", workspace: "josh-personal", query: "private query", maxTokens: 10 })).rejects.toBeInstanceOf(MemoryDClientError);
    } finally {
      globalThis.fetch = original;
    }
  });

  test("aborts the request and body read with the caller signal", async () => {
    const original = globalThis.fetch;
    const { promise, reject } = Promise.withResolvers<Response>();
    globalThis.fetch = (_input, init) => {
      init?.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")), { once: true });
      return promise;
    };
    try {
      const controller = new AbortController();
      const pending = new MemoryDClient(config).recall({
        profile: "personal",
        workspace: "josh-personal",
        query: "q",
        maxTokens: 10,
        signal: controller.signal,
      });
      controller.abort();
      await expect(pending).rejects.toMatchObject({ kind: "cancelled" });
    } finally {
      globalThis.fetch = original;
    }
  });

  test("stale session results cannot commit", async () => {
    const original = globalThis.fetch;
    globalThis.fetch = async () => envelope({ facts: [{ id: "m", content: "context" }], authority: "recall_not_authority" });
    try {
      const session = { sessionId: "s1" };
      const backend = createMemoryDBackend(config);
      await backend.start({ session, settings: {}, agentDir: ".", cwd: ".", taskDepth: 0 });
      const preparation = await backend.beforeAgentStartPrompt?.(session, "prompt");
      await backend.start({ session, settings: {}, agentDir: ".", cwd: ".", taskDepth: 0 });
      expect(preparation?.commit()).toBe(false);
    } finally {
      globalThis.fetch = original;
    }
  });

  test("daemon outage is fail-open and explicit save is the only write path", async () => {
    const original = globalThis.fetch;
    const requests: string[] = [];
    globalThis.fetch = async input => {
      requests.push(String(input));
      throw new TypeError("connection refused");
    };
    try {
      const session = { sessionId: "s1" };
      const backend = createMemoryDBackend(config);
      await backend.start({ session, settings: {}, agentDir: ".", cwd: ".", taskDepth: 0 });
      expect(await backend.beforeAgentStartPrompt?.(session, "prompt")).toBeUndefined();
      const saved = await backend.save?.({ agentDir: ".", cwd: ".", session }, { content: "explicit" });
      expect(saved?.stored).toBe(0);
      expect(requests.some(request => request.includes("/v1/turns"))).toBe(false);
    } finally {
      globalThis.fetch = original;
    }
  });

  test("does not advertise capabilities for an unavailable daemon", async () => {
    const original = globalThis.fetch;
    globalThis.fetch = async () => envelope({
      status: "auth_missing",
      storage: { writable: true },
      features: { recall: true },
    });
    try {
      const session = { sessionId: "s1" };
      const backend = createMemoryDBackend(config);
      await backend.start({ session, settings: {}, agentDir: ".", cwd: ".", taskDepth: 0 });
      await expect(backend.status?.({ agentDir: ".", cwd: ".", session })).resolves.toMatchObject({
        active: false,
        writable: false,
        searchable: false,
      });
    } finally {
      globalThis.fetch = original;
    }
  });
});
