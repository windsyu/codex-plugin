export interface ApiEnvelope<T> {
  apiVersion: string;
  asOfEventSeq: number;
  data: T;
  nextCursor?: string;
}

export interface ProjectRef {
  key: string;
  name: string;
  path: string;
}

export interface ProjectSummary {
  project: ProjectRef;
  threadCount: number;
  currentThreadCount: number;
  lastRecencyAtMs: number;
}

export interface ThreadContextSession {
  baseInstructions?: unknown;
  dynamicTools?: unknown;
  selectedCapabilityRoots?: unknown;
  memoryMode?: string;
  subagentHistoryStartOrdinal?: number;
  multiAgentVersion?: string;
  contextWindow?: unknown;
  agentNickname?: string;
  agentRole?: string;
  agentPath?: string;
  originator?: string;
  cliVersion?: string;
  threadSource?: string;
  historyMode?: string;
  historyBase?: unknown;
  modelProvider?: string;
}

export interface ThreadContextRuntime {
  cwd?: string;
  source?: string;
  model?: string;
  reasoningEffort?: string;
  approvalPolicy?: string;
  approvalsReviewer?: unknown;
  sandbox?: unknown;
  activePermissionProfile?: unknown;
}

export interface ThreadContext {
  session: ThreadContextSession;
  runtime: ThreadContextRuntime;
}

export interface Thread {
  threadKey: string;
  codexThreadId: string;
  storeSourceId: string;
  name?: string;
  cwdDisplay?: string;
  source?: string;
  model?: string;
  archived: boolean;
  status?: string;
  stale: boolean;
  captureCompleteness: string;
  completenessReasons: string[];
  createdAtMs?: number;
  updatedAtMs?: number;
  recencyAtMs?: number;
  lastMessagePreview?: string;
  lastEventSeq: number;
  parentThreadId?: string;
  parentThreadKey?: string;
  forkedFromId?: string;
  forkedFromThreadKey?: string;
  agentNickname?: string;
  agentRole?: string;
  agentPath?: string;
  originator?: string;
  cliVersion?: string;
  threadSource?: string;
  historyMode?: string;
  historyBase?: unknown;
  modelProvider?: string;
  reasoningEffort?: string;
  approvalPolicy?: string;
  approvalsReviewer?: unknown;
  sandbox?: unknown;
  activePermissionProfile?: unknown;
  ruleVersion: string;
  project: ProjectRef;
  context: ThreadContext;
}

export interface TurnContext {
  cwd?: string;
  workspaceRoots?: unknown;
  currentDate?: string;
  timezone?: string;
  approvalPolicy?: string;
  approvalsReviewer?: unknown;
  sandbox?: unknown;
  permissionProfile?: unknown;
  network?: unknown;
  model?: string;
  effort?: unknown;
  personality?: unknown;
  collaborationMode?: unknown;
  multiAgentVersion?: string;
}

export interface Turn {
  turnId: string;
  status: string;
  captureCompleteness: string;
  completenessReasons: string[];
  coverage: unknown;
  startedAtMs?: number;
  completedAtMs?: number;
  executionContext?: unknown;
  context?: TurnContext;
  raw: unknown;
  lastEventSeq: number;
}

export interface Item {
  turnScope: string;
  itemId: string;
  turnId?: string;
  itemType: string;
  status: string;
  startedAtMs?: number;
  completedAtMs?: number;
  summaryText?: string;
  raw: unknown;
  provenance: unknown;
  lastEventSeq: number;
}

export interface RawEvent {
  eventSeq: number;
  eventId: string;
  sourceId: string;
  sourceEpoch: string;
  sourceSeq: number;
  observedAtMs: number;
  eventAtMs?: number;
  threadKey: string;
  codexThreadId: string;
  turnId?: string;
  itemId?: string;
  method: string;
  phase: string;
  durability: string;
  raw: unknown;
  redaction: unknown;
  decodeStatus: string;
  decodeError?: string;
  storedRawHash: string;
  blobId?: string;
}

export interface Relation {
  threadKey: string;
  codexThreadId?: string;
  name?: string;
  status?: string;
  stale?: boolean;
  captureCompleteness?: string;
  resolved: boolean;
}

export interface ThreadDetail {
  thread: Thread;
  sources: unknown[];
  coverageSummary: Record<string, number>;
  pendingRequests: PendingRequest[];
  projectionConflicts: ProjectionConflict[];
  relations: {
    parent?: Relation | null;
    forkedFrom?: Relation | null;
    children: Relation[];
  };
  diagnostics: {
    decodeErrors: number;
    unknownVariants: number;
    conflicts: number;
  };
}

export interface Source {
  sourceId: string;
  kind: string;
  stableIdentity: string;
  status: string;
  lastSeenAtMs?: number;
  currentEpoch?: {
    eventCount?: number;
    sequenceGapCount?: number;
    decodeErrorCount?: number;
    unknownEventCount?: number;
  };
}

export interface PendingRequest {
  sourceId?: string;
  epochId?: string;
  requestId?: string;
  requestType?: string;
  state?: string;
  requestEventSeq?: number;
  resolvedEventSeq?: number;
}

export interface ProjectionConflict {
  conflictId?: string;
  entityType?: string;
  entityKey?: string;
  fieldName?: string;
  status?: string;
  detectedAtMs?: number;
  resolvedAtMs?: number;
}

export interface SearchResult {
  entityKey: string;
  threadKey: string;
  itemId: string;
  turnId?: string;
  snippet: string;
  score: number;
  lastEventSeq: number;
}

export interface Health {
  asOfEventSeq: number;
  status: string;
  ready: boolean;
  database: unknown;
  writer: unknown;
  ingest: unknown;
  consumers: unknown;
  continuity: unknown;
  sources: unknown[];
  live: unknown;
  privacy?: {
    legacyRedactionEvents?: number;
    warning?: string | null;
  };
}
