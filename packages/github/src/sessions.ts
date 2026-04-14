import type { Octokit } from "octokit";
import type { SessionInfo, TurnInfo, Turn } from "@concats/core";
import { parseTurn, suggestSubject } from "@concats/core";

const MAX_SESSION_DEPTH = 200;
const SESSION_REF_PREFIX = "refs/agent/sessions/";

export interface RawCommit {
  sha: string;
  message: string;
  treeSha: string;
  parents: string[];
  authorDate: string;
}

export type FetchCommit = (sha: string) => Promise<RawCommit>;

export interface SessionHistory {
  commits: RawCommit[];
}

export async function collectSessionHistory(
  fetchCommit: FetchCommit,
  tipSha: string,
  sessionId: string,
  maxDepth: number = MAX_SESSION_DEPTH,
): Promise<SessionHistory> {
  const commits: RawCommit[] = [];
  let current: RawCommit;
  try {
    current = await fetchCommit(tipSha);
  } catch {
    return { commits };
  }

  for (let depth = 0; depth < maxDepth; depth++) {
    if (!isTurnFor(current.message, sessionId)) break;
    commits.push(current);
    if (current.parents.length === 0) break;
    const parentSha = current.parents[0];
    try {
      current = await fetchCommit(parentSha);
    } catch {
      break;
    }
    if (!isTurnFor(current.message, sessionId)) break;
  }
  return { commits };
}

export async function loadSessionRefs(
  octokit: Octokit,
  owner: string,
  repo: string,
): Promise<SessionInfo[]> {
  const refs = await octokit.paginate(octokit.rest.git.listMatchingRefs, {
    owner,
    repo,
    ref: "agent/sessions",
  });

  const fetchCommit = octokitFetcher(octokit, owner, repo);

  const loaded = await Promise.all(
    refs.map((ref) => loadOneSession(fetchCommit, ref.ref, ref.object.sha)),
  );

  return loaded
    .filter((s): s is SessionInfo => s !== null)
    .sort(
      (a, b) =>
        new Date(b.timestamp).getTime() - new Date(a.timestamp).getTime(),
    );
}

export async function loadSessionTurns(
  octokit: Octokit,
  owner: string,
  repo: string,
  sessionId: string,
  tipSha: string,
): Promise<TurnInfo[]> {
  const fetchCommit = octokitFetcher(octokit, owner, repo);
  const { commits } = await collectSessionHistory(
    fetchCommit,
    tipSha,
    sessionId,
  );
  return turnsFromHistory(commits);
}

async function loadOneSession(
  fetchCommit: FetchCommit,
  refName: string,
  tipSha: string,
): Promise<SessionInfo | null> {
  const sessionId = refName.replace(SESSION_REF_PREFIX, "");
  const { commits } = await collectSessionHistory(
    fetchCommit,
    tipSha,
    sessionId,
  );
  if (commits.length === 0) return null;

  const tipCommit = commits[0];
  const summary = summarizeSession(commits);

  return {
    id: sessionId,
    title: summary.title,
    timestamp: tipCommit.authorDate,
    turnCount: summary.turnCount,
    tipOid: tipSha,
  };
}

function summarizeSession(commits: RawCommit[]): {
  turnCount: number;
  title: string;
} {
  let turnCount = 0;
  let firstTurnMessage: string | null = null;
  for (const commit of commits) {
    const turn = parseTurn(commit.message);
    if (turn && hasResponseEntry(turn)) {
      turnCount++;
      firstTurnMessage = commit.message;
    }
  }
  const title = firstTurnMessage
    ? (suggestSubject(firstTurnMessage) ?? "(empty prompt)")
    : "(empty prompt)";
  return { turnCount, title };
}

export function turnsFromHistory(commits: RawCommit[]): TurnInfo[] {
  const turns: TurnInfo[] = [];
  for (let i = commits.length - 1; i >= 0; i--) {
    const commit = commits[i];
    const turn = parseTurn(commit.message);
    if (!turn || !hasResponseEntry(turn)) continue;
    turns.push({
      turnNumber: turns.length,
      turn,
      commitOid: commit.sha,
      treeSha: commit.treeSha,
      branchParentSha: commit.parents[1] ?? null,
    });
  }
  return turns;
}

function hasResponseEntry(turn: Turn): boolean {
  return turn.entries.some((e) => e.kind === "response");
}

function isTurnFor(message: string, sessionId: string): boolean {
  const turn = parseTurn(message);
  return turn !== null && turn.sessionId === sessionId;
}

function octokitFetcher(
  octokit: Octokit,
  owner: string,
  repo: string,
): FetchCommit {
  return async (sha) => {
    const { data } = await octokit.rest.git.getCommit({
      owner,
      repo,
      commit_sha: sha,
    });
    return {
      sha: data.sha,
      message: data.message,
      treeSha: data.tree.sha,
      parents: data.parents.map((p) => p.sha),
      authorDate: data.author.date,
    };
  };
}
