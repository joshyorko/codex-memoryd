export type MemoryBackendId = "codex-memoryd";

export interface SessionLike {
  readonly sessionId?: string;
  readonly cwd?: string;
}

export interface SettingsLike {
  readonly [key: string]: unknown;
}

export interface MemoryBackendStatus {
  backend: MemoryBackendId;
  active: boolean;
  writable: boolean;
  searchable: boolean;
  message?: string;
  error?: string;
}

export interface MemoryBackendSearchOptions {
  limit?: number;
  signal?: AbortSignal;
}

export interface MemoryBackendSearchItem {
  id?: string;
  content: string;
  source?: string;
  timestamp?: string;
  score?: number;
}

export interface MemoryBackendSearchResult {
  backend: MemoryBackendId;
  query: string;
  count: number;
  items: MemoryBackendSearchItem[];
  message?: string;
}

export interface MemoryBackendSaveInput {
  content: string;
  context?: string;
  source?: string;
  importance?: number;
}

export interface MemoryBackendSaveResult {
  backend: MemoryBackendId;
  stored: number;
  ids?: string[];
  message?: string;
}

export interface PromptPreparation {
  context?: string;
  commit(): boolean;
}

export interface BackendOperationContext {
  agentDir: string;
  cwd: string;
  session?: SessionLike;
}

export interface BackendFactoryContext {
  session: SessionLike;
  settings: SettingsLike;
  agentDir: string;
  cwd: string;
  taskDepth: number;
  parent?: unknown;
}

export interface MemoryBackend {
  readonly id: MemoryBackendId;
  start(context: BackendFactoryContext): void | Promise<void>;
  buildDeveloperInstructions(
    agentDir: string,
    settings: SettingsLike,
    session?: SessionLike,
  ): Promise<string | undefined>;
  clear(agentDir: string, cwd: string, session?: SessionLike): Promise<void>;
  enqueue(agentDir: string, cwd: string, session?: SessionLike): Promise<void>;
  status?(context: BackendOperationContext): Promise<MemoryBackendStatus>;
  search?(
    context: BackendOperationContext,
    query: string,
    options?: MemoryBackendSearchOptions,
  ): Promise<MemoryBackendSearchResult>;
  save?(context: BackendOperationContext, input: MemoryBackendSaveInput): Promise<MemoryBackendSaveResult>;
  beforeAgentStartPrompt?(
    session: SessionLike,
    promptText: string,
    signal?: AbortSignal,
  ): Promise<PromptPreparation | undefined>;
  preCompactionContext?(
    messages: readonly unknown[],
    settings: SettingsLike,
    session?: SessionLike,
  ): Promise<string | undefined>;
}

export interface RegisteredMemoryBackend {
  id: MemoryBackendId;
  label: string;
  description: string;
  settings: readonly {
    id: string;
    type: "string" | "boolean" | "number";
    description: string;
    default?: string | boolean | number;
    required?: boolean;
  }[];
  capabilities: {
    recall: boolean;
    search: boolean;
    explicitSave: boolean;
    automaticObservation: boolean;
    compactionRecall: boolean;
  };
  create(context: BackendFactoryContext): MemoryBackend;
}

export interface MemoryBackendRegistrationApi {
  registerMemoryBackend(backend: RegisteredMemoryBackend): void;
}

export interface MemoryDConfig {
  baseUrl: string;
  profile: string;
  workspace: string;
  autoRecall: boolean;
  autoObserve: false;
  recallTimeoutMs: number;
  maxTokens: number;
  maxResponseBytes: number;
}
