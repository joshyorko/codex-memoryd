import { createMemoryDBackend, createUnavailableMemoryDBackend } from "./backend";
import { loadConfig, MemoryDConfigError } from "./config";
import type { MemoryBackendRegistrationApi, MemoryDConfig, RegisteredMemoryBackend } from "./types";

export { createMemoryDBackend } from "./backend";
export { MemoryDClient, MemoryDClientError } from "./client";
export { DEFAULT_CONFIG, loadConfig, tryLoadConfig, MemoryDConfigError } from "./config";
export { formatRecall } from "./format";
export type * from "./types";

export const registration: RegisteredMemoryBackend = {
  id: "codex-memoryd",
  label: "MemoryD",
  description: "Local codex-memoryd contextual memory with bounded fail-open recall",
  settings: [
    { id: "codexMemoryd.baseUrl", type: "string", description: "Loopback MemoryD URL", default: "http://127.0.0.1:8989", required: true },
    { id: "codexMemoryd.profile", type: "string", description: "Explicit MemoryD profile", required: true },
    { id: "codexMemoryd.workspace", type: "string", description: "Explicit MemoryD workspace", required: true },
    { id: "codexMemoryd.autoRecall", type: "boolean", description: "Recall before each user turn", default: true },
    { id: "codexMemoryd.autoObserve", type: "boolean", description: "Automatic host observation (disabled until MemoryD #233)", default: false },
    { id: "codexMemoryd.recallTimeoutMs", type: "number", description: "Aggregate recall deadline", default: 500 },
    { id: "codexMemoryd.maxTokens", type: "number", description: "Maximum rendered recall tokens", default: 1200 },
  ],
  capabilities: {
    recall: true,
    search: true,
    explicitSave: true,
    automaticObservation: false,
    compactionRecall: true,
  },
  create(context) {
    const loaded = safeConfigFromSettings(context.settings);
    if (loaded.config) return createMemoryDBackend(loaded.config);
    return createUnavailableMemoryDBackend(loaded.error.message);
  },
};

/**
 * Register through OMP's public backend seam once available. This is deliberately
 * the only host integration entrypoint; no extension event or shadow tool path is
 * provided while the upstream seam is absent.
 */
export function registerMemoryD(api: MemoryBackendRegistrationApi): void {
  api.registerMemoryBackend(registration);
}

export function configFromSettings(settings: Record<string, unknown>): MemoryDConfig {
  return loadConfig({
    baseUrl: settings["codexMemoryd.baseUrl"] as string | undefined,
    profile: settings["codexMemoryd.profile"] as string | undefined,
    workspace: settings["codexMemoryd.workspace"] as string | undefined,
    autoRecall: settings["codexMemoryd.autoRecall"] as boolean | undefined,
    autoObserve: settings["codexMemoryd.autoObserve"],
    recallTimeoutMs: settings["codexMemoryd.recallTimeoutMs"] as number | undefined,
    maxTokens: settings["codexMemoryd.maxTokens"] as number | undefined,
  });
}

export function safeConfigFromSettings(settings: Record<string, unknown>):
  | { config: MemoryDConfig; error?: undefined }
  | { config?: undefined; error: MemoryDConfigError } {
  try {
    return { config: configFromSettings(settings) };
  } catch (error) {
    if (error instanceof MemoryDConfigError) return { error };
    throw error;
  }
}
