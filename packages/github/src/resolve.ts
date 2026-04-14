import type { Octokit } from "octokit";
import type { MatchableCommit } from "@concats/core";

export async function resolveCommitTrees(
  octokit: Octokit,
  owner: string,
  repo: string,
  commitShas: string[],
): Promise<MatchableCommit[]> {
  const results = await Promise.all(
    commitShas.map(async (sha) => {
      const { data: commit } = await octokit.rest.git.getCommit({
        owner,
        repo,
        commit_sha: sha,
      });
      return {
        sha,
        treeSha: commit.tree.sha,
        parentShas: commit.parents.map((p) => p.sha),
      };
    }),
  );
  return results;
}
