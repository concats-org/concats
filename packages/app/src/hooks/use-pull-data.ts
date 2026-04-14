import {
  createContext,
  useContext,
  useState,
  useEffect,
  useCallback,
  useMemo,
  createElement,
} from "react";
import type { ReactNode } from "react";
import {
  createOctokit,
  loadSessionRefs,
  loadSessionTurns,
  resolveCommitTrees,
  matchCommitsToSessions,
} from "@concats/github";
import type {
  Octokit,
  SessionInfo,
  TurnInfo,
  CommitMatch,
  MatchableCommit,
} from "@concats/github";

interface GithubContextValue {
  octokit: Octokit;
  token: string | null;
  setToken: (token: string | null) => void;
}

const GithubContext = createContext<GithubContextValue | null>(null);

export function GithubProvider({ children }: { children: ReactNode }) {
  const [token, setToken] = useState<string | null>(() =>
    sessionStorage.getItem("github_token"),
  );

  const octokit = useMemo(
    () => createOctokit(token ?? undefined),
    [token],
  );

  const handleSetToken = useCallback((newToken: string | null) => {
    setToken(newToken);
    if (newToken) {
      sessionStorage.setItem("github_token", newToken);
    } else {
      sessionStorage.removeItem("github_token");
    }
  }, []);

  return createElement(
    GithubContext.Provider,
    { value: { octokit, token, setToken: handleSetToken } },
    children,
  );
}

export function useGithub() {
  const ctx = useContext(GithubContext);
  if (!ctx) throw new Error("useGithub must be used within GithubProvider");
  return ctx;
}

interface AsyncState<T> {
  data: T | null;
  loading: boolean;
  error: string | null;
}

type PullData = Awaited<
  ReturnType<Octokit["rest"]["pulls"]["get"]>
>["data"];

type PullCommit = Awaited<
  ReturnType<Octokit["rest"]["pulls"]["listCommits"]>
>["data"][number];

export type { PullData, PullCommit };

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function useAsyncResource<T, D extends readonly unknown[]>(
  load: () => Promise<T>,
  deps: D,
): AsyncState<T> {
  const [state, setState] = useState<AsyncState<T>>({
    data: null,
    loading: true,
    error: null,
  });

  useEffect(() => {
    let cancelled = false;
    setState({ data: null, loading: true, error: null });
    load().then(
      (data) => {
        if (!cancelled) setState({ data, loading: false, error: null });
      },
      (err: unknown) => {
        if (!cancelled)
          setState({ data: null, loading: false, error: errorMessage(err) });
      },
    );
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);

  return state;
}

export function usePull(owner: string, repo: string, pullNumber: number) {
  const { octokit } = useGithub();
  return useAsyncResource(
    () =>
      octokit.rest.pulls
        .get({ owner, repo, pull_number: pullNumber })
        .then(({ data }) => data),
    [octokit, owner, repo, pullNumber],
  );
}

export function usePullCommits(
  owner: string,
  repo: string,
  pullNumber: number,
) {
  const { octokit } = useGithub();
  return useAsyncResource<PullCommit[], [Octokit, string, string, number]>(
    () =>
      octokit.paginate(octokit.rest.pulls.listCommits, {
        owner,
        repo,
        pull_number: pullNumber,
      }),
    [octokit, owner, repo, pullNumber],
  );
}

interface SessionContext {
  sessions: SessionInfo[];
  turns: Map<string, TurnInfo[]>;
  matches: CommitMatch[];
}

async function loadAllTurns(
  octokit: Octokit,
  owner: string,
  repo: string,
  sessions: SessionInfo[],
): Promise<Map<string, TurnInfo[]>> {
  const entries = await Promise.all(
    sessions.map(
      async (session) =>
        [
          session.id,
          await loadSessionTurns(
            octokit,
            owner,
            repo,
            session.id,
            session.tipOid,
          ),
        ] as const,
    ),
  );
  return new Map(entries);
}

async function resolveMatchableCommits(
  octokit: Octokit,
  owner: string,
  repo: string,
  commits: PullCommit[],
): Promise<MatchableCommit[]> {
  try {
    return await resolveCommitTrees(
      octokit,
      owner,
      repo,
      commits.map((c) => c.sha),
    );
  } catch {
    return commits.map(({ sha }) => ({ sha, treeSha: "", parentShas: [] }));
  }
}

async function loadSessionContext(
  octokit: Octokit,
  owner: string,
  repo: string,
  commits: PullCommit[],
): Promise<SessionContext> {
  const sessions = await loadSessionRefs(octokit, owner, repo);
  if (sessions.length === 0) {
    return { sessions, turns: new Map(), matches: [] };
  }
  const [turns, matchable] = await Promise.all([
    loadAllTurns(octokit, owner, repo, sessions),
    resolveMatchableCommits(octokit, owner, repo, commits),
  ]);
  const matches = matchCommitsToSessions(matchable, sessions, turns);
  return { sessions, turns, matches };
}

const EMPTY_SESSIONS: AsyncState<SessionInfo[]> = {
  data: null,
  loading: false,
  error: null,
};

export function useSessionsAndMatches(
  owner: string,
  repo: string,
  commits: PullCommit[] | null,
) {
  const { octokit } = useGithub();
  const [sessions, setSessions] =
    useState<AsyncState<SessionInfo[]>>(EMPTY_SESSIONS);
  const [turns, setTurns] = useState<Map<string, TurnInfo[]>>(new Map());
  const [matches, setMatches] = useState<CommitMatch[]>([]);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!commits || commits.length === 0) return;

    let cancelled = false;
    setLoading(true);
    setSessions({ data: null, loading: true, error: null });

    loadSessionContext(octokit, owner, repo, commits).then(
      (ctx) => {
        if (cancelled) return;
        setSessions({ data: ctx.sessions, loading: false, error: null });
        setTurns(ctx.turns);
        setMatches(ctx.matches);
        setLoading(false);
      },
      (err: unknown) => {
        if (cancelled) return;
        setSessions({ data: null, loading: false, error: errorMessage(err) });
        setLoading(false);
      },
    );

    return () => {
      cancelled = true;
    };
  }, [octokit, owner, repo, commits]);

  return { sessions, turns, matches, loading };
}
