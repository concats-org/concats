export type {
  Turn,
  TurnEntryKind,
  TurnToolKind,
  Snapshot,
} from "@concats/message";

import type { Turn } from "@concats/message";

export interface SessionInfo {
  id: string;
  title: string;
  timestamp: string;
  turnCount: number;
  tipOid: string;
}

export interface TurnInfo {
  turnNumber: number;
  turn: Turn;
  commitOid: string;
  treeSha: string;
  branchParentSha: string | null;
}

export interface CommitMatch {
  commitSha: string;
  sessionId: string;
  turnNumber: number;
  matchType: "branch" | "tree";
  confidence: number;
}
