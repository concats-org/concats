import type { SessionInfo, TurnInfo, CommitMatch } from "./types";

export interface MatchableCommit {
  sha: string;
  treeSha: string;
  parentShas: string[];
}

const BRANCH_CONFIDENCE = 1;
const TREE_CONFIDENCE = 1;
const FILL_CONFIDENCE = 0.7;

export function matchCommitsToSessions(
  prCommits: MatchableCommit[],
  sessions: SessionInfo[],
  allTurns: Map<string, TurnInfo[]>,
): CommitMatch[] {
  const matches: CommitMatch[] = [];
  for (const session of sessions) {
    matches.push(...matchSession(session, allTurns, prCommits));
  }
  return matches;
}

interface TurnLink {
  commitSha: string;
  matchType: "branch" | "tree";
  confidence: number;
}

function matchSession(
  session: SessionInfo,
  allTurns: Map<string, TurnInfo[]>,
  prCommits: MatchableCommit[],
): CommitMatch[] {
  const turns = allTurns.get(session.id) ?? [];
  const turnLinks = directLinksByTurn(turns, prCommits);
  if (turnLinks.size === 0) return [];

  // PR commits are ahead-of-base, so a direct parent[1] / tree match is
  // always a commit produced during the session (never the merge-point at
  // base). One linked turn means the whole session belongs to this PR — emit
  // matches for every turn so the timeline renders the session in full.
  const fillCommit = earliestLinkedCommit(prCommits, turnLinks);
  return turns.map((turn) => {
    const link = turnLinks.get(turn.turnNumber);
    return link
      ? {
          commitSha: link.commitSha,
          sessionId: session.id,
          turnNumber: turn.turnNumber,
          matchType: link.matchType,
          confidence: link.confidence,
        }
      : {
          commitSha: fillCommit,
          sessionId: session.id,
          turnNumber: turn.turnNumber,
          matchType: "branch",
          confidence: FILL_CONFIDENCE,
        };
  });
}

function directLinksByTurn(
  turns: TurnInfo[],
  prCommits: MatchableCommit[],
): Map<number, TurnLink> {
  const prByBranchSha = new Map(prCommits.map((c) => [c.sha, c]));
  const prByTreeSha = new Map(prCommits.map((c) => [c.treeSha, c]));
  const links = new Map<number, TurnLink>();
  for (const turn of turns) {
    if (turn.branchParentSha) {
      const prCommit = prByBranchSha.get(turn.branchParentSha);
      if (prCommit) {
        links.set(turn.turnNumber, {
          commitSha: prCommit.sha,
          matchType: "branch",
          confidence: BRANCH_CONFIDENCE,
        });
        continue;
      }
    }
    const treePr = prByTreeSha.get(turn.treeSha);
    if (treePr) {
      links.set(turn.turnNumber, {
        commitSha: treePr.sha,
        matchType: "tree",
        confidence: TREE_CONFIDENCE,
      });
    }
  }
  return links;
}

function earliestLinkedCommit(
  prCommits: MatchableCommit[],
  turnLinks: Map<number, TurnLink>,
): string {
  const linkedShas = new Set(
    Array.from(turnLinks.values()).map((l) => l.commitSha),
  );
  const earliest = prCommits.find((c) => linkedShas.has(c.sha));
  return earliest?.sha ?? prCommits[0]!.sha;
}
