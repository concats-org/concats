import {
  initSync,
  parse_turn,
  parse_snapshot,
  suggest_subject,
} from "./wasm/concats_message.js";
import { wasmBase64 } from "./wasm/inline.js";

export type TurnToolKind =
  | "read"
  | "edit"
  | "delete"
  | "move"
  | "search"
  | "execute"
  | "think"
  | "fetch"
  | "switch_mode"
  | "other";

export type TurnEntryKind =
  | { kind: "prompt"; text: string }
  | { kind: "response"; text: string }
  | { kind: "tool_call"; toolKind: TurnToolKind };

export interface Turn {
  subject: string;
  sessionId: string;
  agentName: string | null;
  entries: TurnEntryKind[];
}

export interface Snapshot {
  sessionId: string;
  reason:
    | "turn_commit"
    | "turn_amend"
    | "tool_write"
    | "files_changed"
    | null;
}

let initialized = false;

declare function atob(data: string): string;

function decodeBase64(b64: string): ArrayBuffer {
  const raw = atob(b64);
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) {
    bytes[i] = raw.charCodeAt(i);
  }
  return bytes.buffer;
}

function ensureInit(): void {
  if (initialized) return;
  initSync({ module: decodeBase64(wasmBase64) });
  initialized = true;
}

export function parseTurn(input: string): Turn | null {
  ensureInit();
  const result = parse_turn(input);
  return result ?? null;
}

export function parseSnapshot(input: string): Snapshot | null {
  ensureInit();
  const result = parse_snapshot(input);
  return result ?? null;
}

export function suggestSubject(input: string): string | null {
  ensureInit();
  return suggest_subject(input) ?? null;
}
