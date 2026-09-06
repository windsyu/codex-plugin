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
  project?: ProjectRef;
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
  requestKey: string;
  sourceId: string;
  sourceEpoch: string;
  requestId: string;
  requestType?: string;
  state: string;
  requestVersion: number;
  payload: Record<string, unknown>;
  requestEventSeq?: number;
  resolvedEventSeq?: number;
}

export interface ControllerSource {
  sourceId: string;
  sourceEpoch: string;
  supervisorVersion: number;
  state: string;
  unavailableReason?: string;
}

export interface SessionSource {
  storeSourceId: string;
  sourceId: string;
  sourceEpoch: string;
  supervisorVersion: number;
  defaultCwd: string;
  status: 'ready' | 'unavailable';
}

export interface SlashCommand {
  name: string;
  capability: string;
  interactionRequiredWithoutArgument: boolean;
}

export interface ControlCatalog {
  sourceId: string;
  sourceEpoch: string;
  threadLoaded: boolean;
  activeTurnId?: string;
  collaborationMode?: { mode?: string; settings?: Record<string, unknown> };
  goal?: { objective?: string; status?: string; tokensUsed?: number; timeUsedSeconds?: number };
  capabilities: { entries: Record<string, {
    available: boolean;
    experimental: boolean;
    data?: { data?: Array<Record<string, unknown>> };
    errorCode?: string;
  }> };
  slashCommands: SlashCommand[];
}

export interface GatewayCommand {
  commandId: string;
  capability?: string;
  state: string;
  error?: { code: string; message: string };
  result?: unknown;
}

export interface GatewayStatusCard {
  kind: 'gatewayStatusCard';
  cardType: 'status' | 'mcp' | 'usage';
  sourceId: string;
  sourceEpoch: string;
  threadKey: string;
  content: unknown;
}

export interface ImageUpload { uploadId: string; mimeType: string; sizeBytes: number; expiresAtMs: number; }

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
  control?: { enabled: boolean; tailscaleMutationAccess: boolean; warning?: string | null };
  sessionKernel?: SessionKernelCapabilities;
  privacy?: {
    legacyRedactionEvents?: number;
    warning?: string | null;
  };
}

export interface SessionKernelCapabilities {
  configuredMode: 'off' | 'preview' | 'tui';
  compiled: boolean;
  workerAvailable: boolean;
  fakeCliAvailable: boolean;
  cliAvailable: boolean;
  errorCode?: string;
}

export type SessionWorkerState = 'starting' | 'connecting' | 'ready' | 'detached' | 'stopping' | 'exited' | 'stale_epoch' | 'failed';

export interface InputLease {
  leaseId?: string;
  ownerAttachmentId?: string;
  version: number;
  state: 'none' | 'active' | 'releasing' | 'expired' | 'stale';
}

export interface SessionWorker {
  workerId: string;
  state: SessionWorkerState;
  pid?: number;
  cwd: string;
  rows: number;
  cols: number;
  outputSeq: number;
  terminalRetainedBytes: number;
  terminalCheckpointBytes: number;
  ptyEof: boolean;
  exit?: { success: boolean; exitCode: number; signal?: string };
  errorCode?: string;
  inputLease: InputLease;
  persistedWorker?: {
    version: number;
    primaryThreadId?: string;
    sourceId: string;
    sourceEpoch: string;
    mode: 'new' | 'resume';
    canonicalCwd: string;
  };
  threadLeases?: Array<{
    leaseId: string;
    codexThreadId?: string;
    reservationId?: string;
    role: 'primary' | 'side' | 'child';
    state: string;
    version: number;
  }>;
  activeTurns?: Array<{
    codexThreadId: string;
    codexTurnId: string;
    ownerType: 'terminal' | 'channel' | 'gateway';
    ownerId: string;
    principalId: string;
    state: string;
    version: number;
  }>;
}

// The terminal WebSocket intentionally carries only the Worker actor's volatile snapshot.
// Durable/session metadata is supplied by the REST/SSE SessionWorker view and must survive
// every terminal state update.
export type SessionWorkerSnapshot = Omit<SessionWorker, 'persistedWorker' | 'threadLeases' | 'activeTurns'>;

export interface SessionAttachment {
  attachmentId: string;
  attachmentToken: string;
  descriptor: string;
  descriptorExpiresInSeconds: number;
  inputLeaseVersion: number;
}

export interface TerminalSnapshotFrame {
  type: 'snapshot';
  checkpointSeq: number;
  fromSeq: number;
  toSeq: number;
  rows: number;
  cols: number;
  screen: string;
  replay: string;
  encoding: 'base64';
  complete: boolean;
  truncated: boolean;
}
