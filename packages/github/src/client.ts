import { Octokit } from "octokit";

export type { Octokit };

export function createOctokit(token?: string): Octokit {
  return new Octokit(token ? { auth: token } : {});
}
