import { describe, expect, it } from "bun:test";
import {
  collectSessionHistory,
  turnsFromHistory,
  type FetchCommit,
  type RawCommit,
} from "./sessions";

function turnMessage(options: {
  subject?: string;
  prompt?: string;
  response?: string;
  sessionId: string;
  agent?: string;
}): string {
  const parts: string[] = [options.subject ?? "turn", ""];
  const entries: string[] = [];
  if (options.prompt) entries.push(`<prompt>${options.prompt}</prompt>`);
  if (options.response) entries.push(`<response>${options.response}</response>`);
  if (entries.length > 0) {
    parts.push(entries.join("\n"));
    parts.push("");
  }
  parts.push(`Session: ${options.sessionId}`);
  if (options.agent) parts.push(`Agent: ${options.agent}`);
  return parts.join("\n");
}

function snapshotMessage(sessionId: string, reason?: string): string {
  const parts = ["snapshot", "", `Session: ${sessionId}`];
  if (reason) parts.push(`Reason: ${reason}`);
  return parts.join("\n");
}

function fakeFetcher(commits: RawCommit[]): FetchCommit {
  const byId = new Map(commits.map((c) => [c.sha, c]));
  return async (sha) => {
    const commit = byId.get(sha);
    if (!commit) throw new Error(`unknown sha ${sha}`);
    return commit;
  };
}

function commit(
  sha: string,
  message: string,
  parents: string[] = [],
  treeSha = `tree-${sha}`,
): RawCommit {
  return {
    sha,
    message,
    treeSha,
    parents,
    authorDate: "2026-01-01T00:00:00Z",
  };
}

describe("collectSessionHistory", () => {
  it("walks a linear session chain until it hits a non-session parent", async () => {
    const sessionId = "sess";
    const history = [
      commit("tip", turnMessage({ prompt: "p3", response: "r3", sessionId }), ["mid"]),
      commit("mid", turnMessage({ prompt: "p2", response: "r2", sessionId }), ["root"]),
      commit(
        "root",
        turnMessage({ prompt: "p1", response: "r1", sessionId }),
        ["branch-base"],
      ),
      commit("branch-base", "chore: base\n", []),
    ];

    const { commits } = await collectSessionHistory(
      fakeFetcher(history),
      "tip",
      sessionId,
    );

    expect(commits.map((c) => c.sha)).toEqual(["tip", "mid", "root"]);
  });

  it("stops when a session commit has no parents", async () => {
    const sessionId = "sess";
    const history = [
      commit("tip", turnMessage({ prompt: "p", response: "r", sessionId }), []),
    ];

    const { commits } = await collectSessionHistory(
      fakeFetcher(history),
      "tip",
      sessionId,
    );

    expect(commits.map((c) => c.sha)).toEqual(["tip"]);
  });

  it("stops at a turn whose Session trailer belongs to a different session", async () => {
    const sessionId = "sess";
    const otherId = "other";
    const history = [
      commit(
        "tip",
        turnMessage({ prompt: "p", response: "r", sessionId }),
        ["forked-from"],
      ),
      commit(
        "forked-from",
        turnMessage({ prompt: "p", response: "r", sessionId: otherId }),
        ["base"],
      ),
      commit("base", "feat: x\n", []),
    ];

    const { commits } = await collectSessionHistory(
      fakeFetcher(history),
      "tip",
      sessionId,
    );

    expect(commits.map((c) => c.sha)).toEqual(["tip"]);
  });

  it("stops at a snapshot commit since session refs walk only turns", async () => {
    const sessionId = "sess";
    const history = [
      commit("tip", turnMessage({ prompt: "p", response: "r", sessionId }), ["snap"]),
      commit("snap", snapshotMessage(sessionId, "turn_commit"), ["base"]),
      commit("base", "feat: x\n", []),
    ];

    const { commits } = await collectSessionHistory(
      fakeFetcher(history),
      "tip",
      sessionId,
    );

    expect(commits.map((c) => c.sha)).toEqual(["tip"]);
  });

  it("returns empty when the tip itself is not a session commit", async () => {
    const history = [commit("tip", "docs: readme\n", [])];

    const { commits } = await collectSessionHistory(
      fakeFetcher(history),
      "tip",
      "sess",
    );

    expect(commits).toEqual([]);
  });

  it("stops cleanly when a parent commit cannot be fetched", async () => {
    const sessionId = "sess";
    const history = [
      commit("tip", turnMessage({ prompt: "p", response: "r", sessionId }), ["missing"]),
    ];

    const { commits } = await collectSessionHistory(
      fakeFetcher(history),
      "tip",
      sessionId,
    );

    expect(commits.map((c) => c.sha)).toEqual(["tip"]);
  });

  it("caps walks at maxDepth", async () => {
    const sessionId = "sess";
    const history: RawCommit[] = [];
    for (let i = 0; i < 10; i++) {
      history.push(
        commit(
          `c${i}`,
          turnMessage({ prompt: `p${i}`, response: `r${i}`, sessionId }),
          i < 9 ? [`c${i + 1}`] : [],
        ),
      );
    }

    const { commits } = await collectSessionHistory(
      fakeFetcher(history),
      "c0",
      sessionId,
      3,
    );

    expect(commits.map((c) => c.sha)).toEqual(["c0", "c1", "c2"]);
  });
});

describe("turnsFromHistory", () => {
  it("numbers turns chronologically and drops entries without responses", async () => {
    const sessionId = "sess";
    const history = [
      commit(
        "tip",
        turnMessage({ prompt: "p2", response: "r2", sessionId }),
        ["mid"],
      ),
      commit("mid", turnMessage({ prompt: "p-orphan", sessionId }), ["root"]),
      commit(
        "root",
        turnMessage({ prompt: "p1", response: "r1", sessionId }),
        [],
      ),
    ];

    const { commits } = await collectSessionHistory(
      fakeFetcher(history),
      "tip",
      sessionId,
    );
    const turns = turnsFromHistory(commits);

    expect(turns).toHaveLength(2);
    expect(turns[0].commitOid).toBe("root");
    expect(turns[0].turnNumber).toBe(0);
    expect(turns[1].commitOid).toBe("tip");
    expect(turns[1].turnNumber).toBe(1);
  });

  it("captures parent[1] as the branch link for turns with two parents", async () => {
    const sessionId = "sess";
    const history = [
      commit(
        "tip",
        turnMessage({ prompt: "p2", response: "r2", sessionId }),
        ["mid", "branch-head"],
      ),
      commit(
        "mid",
        turnMessage({ prompt: "p1", response: "r1", sessionId }),
        ["base"],
      ),
      commit("base", "feat: x\n", []),
    ];

    const { commits } = await collectSessionHistory(
      fakeFetcher(history),
      "tip",
      sessionId,
    );
    const turns = turnsFromHistory(commits);

    expect(turns[0].commitOid).toBe("mid");
    expect(turns[0].branchParentSha).toBeNull();
    expect(turns[1].commitOid).toBe("tip");
    expect(turns[1].branchParentSha).toBe("branch-head");
  });
});
