import type { TextKey } from './reading';

export type ToolResultSource =
  | { kind: 'model_request'; requestId: string; clientRequestIndex: number | null; inputIndex: number }
  | { kind: 'native_rollout'; sourceRef: string; byteOffset: number; nativeItemId: string; processId: string | null };
export interface NativeCommand {
  key: { codexThreadId: string; codexTurnId: string; nativeItemId: string };
  source: { sourceRef: string; byteOffset: number; ordinal: number | null };
  status: 'in_progress' | 'completed' | 'failed' | 'declined';
  processId: string | null; commandSource: string; command: string[]; cwd: string;
  output: string; exitCode: number | null; durationMs: number | null; truncated: boolean; omitted: boolean;
}
export interface NativeFileChange {
  key: { codexThreadId: string; codexTurnId: string; nativeItemId: string };
  source: { sourceRef: string; byteOffset: number; ordinal: number | null };
  status: 'completed' | 'failed' | 'declined';
  files: { path: string; operation: 'add' | 'update' | 'delete' | 'unknown'; moveTo: string | null }[];
  stdout: string | null; stderr: string | null; truncated: boolean; omitted: boolean;
}
export type ToolCategory = 'command' | 'code' | 'patch' | 'other';
export type ProposedPatch =
  | { state: 'ready'; environmentId: string | null; files: ProposedFile[] }
  | { state: 'unavailable'; reason: 'incomplete' | 'truncated' | 'identity_conflict' | 'unsupported_format' | 'budget' };
export interface ProposedFile {
  operation: 'add' | 'update' | 'delete'; path: string; moveTo: string | null;
  addedLines: number; removedLines: number | null;
  sections: { anchor: string | null; atEof: boolean; lines: { kind: 'added' | 'removed' | 'context'; text: string }[] }[];
}
export interface ToolCall {
  key: TextKey; kind: 'tool_call'; toolKind: 'function' | 'custom';
  callId: string | null; name: string | null; namespace: string | null;
  category: ToolCategory; arguments: string; argumentsState: 'receiving' | 'generated' | 'incomplete';
  command: { text: string; cwd: string | null } | null;
  proposedPatch: ProposedPatch | null;
  execution: 'unobserved' | 'running' | 'result_observed' | 'succeeded' | 'failed' | 'declined';
  result: { source: ToolResultSource; output: string; streams?: { stdout: string | null; stderr: string | null }; exitCode: number | null; durationMs: number | null; truncated: boolean; omitted: boolean } | null;
  revision: number; orderIndex: number; captureSeq: number; truncated: boolean;
  identityConflict: boolean; resultConflict: boolean;
}
export interface ToolContext {
  requestId: string; clientRequestIndex: number | null; partial: boolean;
  definitions: { name: string; namespace: string | null; toolKind: string; category: ToolCategory; source: string; inputIndex: number | null }[];
  outputs: { inputIndex: number; toolKind: string; callId: string | null; output: string; truncated: boolean; omitted: boolean }[];
}
