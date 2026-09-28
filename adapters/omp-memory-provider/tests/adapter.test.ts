import { describe, expect, test, vi } from "bun:test";
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
    expect(() => loadConfig({ profile: "personal", workspace: "work/team" })).toThrow("canonical");
    expect(() => loadConfig({ profile: "personal", workspace: " -- " })).toThrow("canonical");
  });

  test("rejects non-boolean observation and recall settings", () => {
    expect(() => loadConfig({ profile: "personal", workspace: "work", autoObserve: "false" })).toThrow("autoObserve must be boolean");
    expect(() => loadConfig({ profile: "personal", workspace: "work", autoRecall: "true" })).toThrow("autoRecall must be boolean");
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
  test("renders withheld-only diagnostics without admitting memory", () => {
    const rendered = formatRecall({ withheld: [{ reason: "policy", count: 2 }, { reason: "quarantine", count: 3 }] }, 1200);
    expect(rendered.count).toBe(0);
    expect(rendered.context).toContain("recall_not_authority");
    expect(rendered.context).toContain("withheld: 5");
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

  test("closes response bodies before HTTP and content-type protocol errors", async () => {
    const original = globalThis.fetch;
    let cancelled = 0;
    globalThis.fetch = async input => {
      const status = String(input).endsWith("/status") ? 500 : 200;
      const body = new ReadableStream<Uint8Array>({
        start(controller) { controller.enqueue(new TextEncoder().encode("error")); },
        cancel() { cancelled += 1; },
      });
      return new Response(body, { status, headers: { "content-type": status === 200 ? "text/plain" : "application/json" } });
    };
    try {
      const client = new MemoryDClient(config);
      await expect(client.status()).rejects.toMatchObject({ kind: "http", status: 500 });
      await expect(client.recall({ profile: "personal", workspace: "josh-personal", query: "q", maxTokens: 10 })).rejects.toMatchObject({ kind: "protocol-mismatch" });
      expect(cancelled).toBe(2);
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
  test("classifies a drip response timeout after headers", async () => {
    vi.useFakeTimers();
    const original = globalThis.fetch;
    globalThis.fetch = async () => new Response(new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode("{"));
        setTimeout(() => controller.error(new DOMException("drip ended", "AbortError")), 60);
      },
    }), { headers: { "content-type": "application/json" } });
    try {
      const pending = new MemoryDClient({ ...config, recallTimeoutMs: 25 }).status();
      vi.advanceTimersByTime(60);
      await expect(pending).rejects.toMatchObject({ kind: "timeout" });
    } finally {
      globalThis.fetch = original;
      vi.useRealTimers();
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
  test("rejects malformed capability status as protocol mismatch", async () => {
    const original = globalThis.fetch;
    globalThis.fetch = async () => envelope({ status: "local_only", storage: {}, features: { recall: true } });
    try {
      const session = { sessionId: "s1" };
      const backend = createMemoryDBackend(config);
      await backend.start({ session, settings: {}, agentDir: ".", cwd: ".", taskDepth: 0 });
      await expect(backend.status?.({ agentDir: ".", cwd: ".", session })).resolves.toMatchObject({ active: false, message: "protocol-mismatch" });
    } finally {
      globalThis.fetch = original;
    }
  });
});

  test("save uses active root scope and preserves repository metadata", async () => {
    const original = globalThis.fetch;
    let body: Record<string, any> | undefined;
    globalThis.fetch = async (_input, init) => {
      body = JSON.parse(String(init?.body));
      return envelope({ record_ids: ["r1"] });
    };
    try {
      const session = { sessionId: "s1", repoId: "repo-new", repo: { repo_id: "repo-new", root: "/repo", remote: "https://example.test/repo.git", branch: "main", commit: "abc", is_git: true } };
      const backend = createMemoryDBackend(config);
      await backend.start({ session, settings: {}, agentDir: ".", cwd: ".", taskDepth: 0 });
      const saved = await backend.save?.({ agentDir: ".", cwd: "." }, { content: "explicit" });
      expect(saved?.stored).toBe(1);
      expect(body?.metadata?.session_id).toBe("s1");
      expect(body?.repo?.repo_id).toBe("repo-new");
      expect(body?.repo).toMatchObject({ repo_id: "repo-new", root: "/repo", remote: "https://example.test/repo.git", branch: "main", commit: "abc", is_git: true });

      await new MemoryDClient(config).explicitSave({
        profile: "personal",
        workspace: "josh-personal",
        content: "explicit",
        repoId: "repo-new",
        repo: { branch: "main", commit: "abc" },
      });
      expect(body?.repo).toEqual({ branch: "main", commit: "abc", repo_id: "repo-new" });
    } finally {
      globalThis.fetch = original;
    }
  });
  test("counts an accepted conclusion when daemon omits a derived record id", async () => {
    const original = globalThis.fetch;
    globalThis.fetch = async () => envelope({ created: ["concl_1"], record_ids: [], rejected: [] });
    try {
      const backend = createMemoryDBackend(config);
      await backend.start({ session: { sessionId: "s1" }, settings: {}, agentDir: ".", cwd: ".", taskDepth: 0 });
      await expect(backend.save?.({ agentDir: ".", cwd: "." }, { content: "accepted" })).resolves.toMatchObject({
        stored: 1,
        ids: ["concl_1"],
      });
    } finally {
      globalThis.fetch = original;
    }
  });

  test("counts Unicode code points for the explicit-save limit", async () => {
    const original = globalThis.fetch;
    let calls = 0;
    globalThis.fetch = async () => {
      calls += 1;
      return envelope({ record_ids: ["r1"] });
    };
    try {
      const backend = createMemoryDBackend(config);
      const context = { agentDir: ".", cwd: "." };
      expect((await backend.save?.(context, { content: "🙂".repeat(16_000) }))?.stored).toBe(1);
      expect((await backend.save?.(context, { content: "🙂".repeat(16_001) }))?.stored).toBe(0);
      expect(calls).toBe(1);
    } finally {
      globalThis.fetch = original;
    }
  });

  test("keeps bounded text truncation on Unicode boundaries", async () => {
    const original = globalThis.fetch;
    const bodies = new Map<string, any[]>();
    const boundary = (limit: number) => "a".repeat(limit - 2) + "🙂" + "a";
    const suffixBoundary = "a".repeat(8_000) + "🙂";
    globalThis.fetch = async (input, init) => {
      const path = new URL(String(input)).pathname;
      bodies.set(path, [...(bodies.get(path) ?? []), JSON.parse(String(init?.body ?? "{}"))]);
      if (path === "/v1/recall") return envelope({ facts: [], authority: "recall_not_authority" });
      if (path === "/v1/search") return envelope({ matches: [] });
      return envelope({ record_ids: ["r1"] });
    };
    try {
      const session = { sessionId: "s1" };
      const backend = createMemoryDBackend(config);
      await backend.start({ session, settings: {}, agentDir: ".", cwd: ".", taskDepth: 0 });
      await backend.beforeAgentStartPrompt?.(session, boundary(8_001));
      await backend.search?.({ agentDir: ".", cwd: ".", session }, boundary(8_001));
      await backend.preCompactionContext?.([{ content: suffixBoundary }], {}, session);
      await backend.save?.({ agentDir: ".", cwd: ".", session }, { content: "save", context: boundary(2_001), source: boundary(201) });
      const recalls = bodies.get("/v1/recall") ?? [];
      expect(recalls[0]?.query.endsWith("🙂")).toBe(true);
      expect(recalls[1]?.query.endsWith("🙂")).toBe(true);
      expect(bodies.get("/v1/search")?.[0]?.query.endsWith("🙂")).toBe(true);
      expect(bodies.get("/v1/conclusions")?.[0]?.metadata.context.endsWith("🙂")).toBe(true);
      expect(bodies.get("/v1/conclusions")?.[0]?.metadata.source.endsWith("🙂")).toBe(true);
    } finally {
      globalThis.fetch = original;
    }
  });
  test("sessionless enqueue updates the retained root status", async () => {
    const original = globalThis.fetch;
    globalThis.fetch = async () => envelope({
      status: "local_only",
      storage: { writable: true },
      features: { recall: true, search: true },
    });
    try {
      const backend = createMemoryDBackend(config);
      await backend.start({ session: { sessionId: "s1" }, settings: {}, agentDir: ".", cwd: ".", taskDepth: 0 });
      await backend.enqueue?.(".", ".");
      await expect(backend.status?.({ agentDir: ".", cwd: "." })).resolves.toMatchObject({
        active: true,
        message: expect.stringContaining("unsupported-no-queue"),
      });
    } finally {
      globalThis.fetch = original;
    }
  });
