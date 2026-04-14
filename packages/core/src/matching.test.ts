import { describe, expect, it } from "bun:test";
import { matchCommitsToSessions, type MatchableCommit } from "./matching";
import type { SessionInfo, TurnInfo } from "./types";

function session(
  overrides: Partial<SessionInfo> & Pick<SessionInfo, "id">,
): SessionInfo {
  return {
    title: "t",
    timestamp: "2026-01-01T00:00:00Z",
    turnCount: 1,
    tipOid: `${overrides.id}-tip`,
    ...overrides,
  };
}

function turn(
  treeSha: string,
  turnNumber = 0,
  branchParentSha: string | null = null,
): TurnInfo {
  return {
    turnNumber,
    turn: { subject: "t", sessionId: "s", agentName: null, entries: [] },
    commitOid: `c-${treeSha}`,
    treeSha,
    branchParentSha,
  };
}

function commit(sha: string, treeSha: string, parents: string[] = []): MatchableCommit {
  return { sha, treeSha, parentShas: parents };
}

describe("matchCommitsToSessions", () => {
  it("emits a branch match when a turn's branchParentSha is a PR commit", () => {
    const sessions = [session({ id: "sess" })];
    const turns = new Map([["sess", [turn("t0", 0, "pr-sha")]]]);
    const prCommits = [commit("pr-sha", "pr-tree")];

    const matches = matchCommitsToSessions(prCommits, sessions, turns);

    expect(matches).toEqual([
      {
        commitSha: "pr-sha",
        sessionId: "sess",
        turnNumber: 0,
        matchType: "branch",
        confidence: 1,
      },
    ]);
  });

  it("emits a tree match when a PR commit shares a turn tree", () => {
    const sessions = [session({ id: "sess" })];
    const turns = new Map([["sess", [turn("T1")]]]);
    const prCommits = [commit("c1", "T1")];

    const matches = matchCommitsToSessions(prCommits, sessions, turns);

    expect(matches).toEqual([
      {
        commitSha: "c1",
        sessionId: "sess",
        turnNumber: 0,
        matchType: "tree",
        confidence: 1,
      },
    ]);
  });

  it("renders the whole session when at least one turn directly links", () => {
    const sessions = [session({ id: "sess" })];
    const turns = new Map([
      [
        "sess",
        [
          turn("t0", 0, null),
          turn("t1", 1, null),
          turn("t2", 2, "pr-sha"),
        ],
      ],
    ]);
    const prCommits = [commit("pr-sha", "pr-tree")];

    const matches = matchCommitsToSessions(prCommits, sessions, turns);

    expect(matches.map((m) => m.turnNumber)).toEqual([0, 1, 2]);
    expect(matches.every((m) => m.commitSha === "pr-sha")).toBe(true);
    expect(matches[2].confidence).toBe(1);
    expect(matches[0].confidence).toBeLessThan(1);
  });

  it("attaches unlinked turns to the earliest linked PR commit", () => {
    const sessions = [session({ id: "sess" })];
    const turns = new Map([
      ["sess", [turn("t0", 0, null), turn("t1", 1, "pr-b")]],
    ]);
    const prCommits = [commit("pr-a", "tree-a"), commit("pr-b", "tree-b")];

    const matches = matchCommitsToSessions(prCommits, sessions, turns);

    expect(matches[0]).toMatchObject({ turnNumber: 0, commitSha: "pr-b" });
    expect(matches[1]).toMatchObject({ turnNumber: 1, commitSha: "pr-b" });
  });

  it("prefers branch over tree when both link to the same PR commit", () => {
    const sessions = [session({ id: "sess" })];
    const turns = new Map([["sess", [turn("T1", 0, "pr-sha")]]]);
    const prCommits = [commit("pr-sha", "T1")];

    const matches = matchCommitsToSessions(prCommits, sessions, turns);

    expect(matches).toHaveLength(1);
    expect(matches[0].matchType).toBe("branch");
  });

  it("returns no matches when no turn directly links", () => {
    const sessions = [session({ id: "sess" })];
    const turns = new Map([["sess", [turn("ghost", 0, "elsewhere")]]]);
    const prCommits = [commit("c1", "tree1", ["other"])];

    expect(matchCommitsToSessions(prCommits, sessions, turns)).toEqual([]);
  });

  it("does not match a PR via a shared base parent (no anchor fallback)", () => {
    const sessions = [session({ id: "sess" })];
    const turns = new Map([["sess", [turn("ghost", 0, null)]]]);
    const prCommits = [commit("c1", "tree1", ["base"])];

    expect(matchCommitsToSessions(prCommits, sessions, turns)).toEqual([]);
  });
});
