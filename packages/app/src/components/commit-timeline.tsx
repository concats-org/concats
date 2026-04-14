import { FilePlus, GitCommitVertical } from "lucide-react";
import { Skeleton } from "@/components/ui/skeleton";
import type {
  CommitMatch,
  SessionInfo,
  TurnInfo,
  TurnEntryKind,
} from "@concats/github";
import type { PullCommit } from "@/hooks/use-pull-data";

interface CommitTimelineProps {
  commits: PullCommit[] | null;
  matches: CommitMatch[];
  sessions: SessionInfo[] | null;
  turns: Map<string, TurnInfo[]>;
  loading: boolean;
  sessionsLoading: boolean;
}

export function CommitTimeline({
  commits,
  matches,
  sessions,
  turns,
  loading,
  sessionsLoading,
}: CommitTimelineProps) {
  if (loading || sessionsLoading) return <CommitTimelineSkeleton />;
  if (!commits || commits.length === 0) {
    return <p className="text-sm text-muted-foreground">No commits found.</p>;
  }

  const sessionsById = new Map((sessions ?? []).map((s) => [s.id, s]));

  return (
    <div className="flex w-full flex-col gap-2">
      {commits.map((commit) => {
        const commitMatches = matches.filter((m) => m.commitSha === commit.sha);
        const matchedTurns = resolveMatchedTurns(
          commitMatches,
          sessionsById,
          turns,
        );
        return (
          <div key={commit.sha} className="flex w-full flex-col gap-2">
            {matchedTurns.map((turn) => (
              <TurnBlock key={`${turn.commitOid}-${turn.turnNumber}`} turn={turn} />
            ))}
            <CommitRow commit={commit} />
          </div>
        );
      })}
    </div>
  );
}

function CommitTimelineSkeleton() {
  return (
    <div className="flex w-full flex-col gap-4">
      {Array.from({ length: 3 }, (_, i) => (
        <Skeleton key={i} className="h-24 w-full" />
      ))}
    </div>
  );
}

function resolveMatchedTurns(
  commitMatches: CommitMatch[],
  sessionsById: Map<string, SessionInfo>,
  turns: Map<string, TurnInfo[]>,
): TurnInfo[] {
  const seen = new Set<string>();
  const result: TurnInfo[] = [];
  for (const match of commitMatches) {
    if (!sessionsById.has(match.sessionId)) continue;
    const sessionTurns = turns.get(match.sessionId) ?? [];
    const turn = sessionTurns.find((t) => t.turnNumber === match.turnNumber);
    if (!turn) continue;
    const key = `${match.sessionId}-${turn.turnNumber}`;
    if (seen.has(key)) continue;
    seen.add(key);
    result.push(turn);
  }
  return result;
}

function TurnBlock({ turn }: { turn: TurnInfo }) {
  const prompt = turn.turn.entries.find((e) => e.kind === "prompt");
  const response = turn.turn.entries.find((e) => e.kind === "response");
  const tools = turn.turn.entries.filter(
    (e): e is Extract<TurnEntryKind, { kind: "tool_call" }> =>
      e.kind === "tool_call",
  );

  return (
    <div className="flex w-full flex-col gap-2 px-2 md:px-8">
      {prompt && prompt.kind === "prompt" && (
        <div className="flex w-full items-start gap-3 rounded-md p-4">
          <span className="text-lg leading-5">○</span>
          <p className="flex-1 text-sm leading-5 text-foreground">
            {prompt.text}
          </p>
        </div>
      )}
      {response && response.kind === "response" && (
        <div className="flex w-full items-start gap-3 rounded-md p-4">
          <span className="text-lg leading-5">⟡</span>
          <p className="flex-1 text-base leading-6 whitespace-pre-wrap text-foreground">
            {response.text}
          </p>
        </div>
      )}
      {tools.map((tool, i) => (
        <div
          key={i}
          className="flex w-full items-start justify-end gap-2.5"
        >
          <span className="text-sm leading-5 text-foreground">
            {tool.toolKind}
          </span>
          <FilePlus className="size-6 shrink-0" />
        </div>
      ))}
    </div>
  );
}

function CommitRow({ commit }: { commit: PullCommit }) {
  const firstLine = commit.commit.message.split("\n")[0];
  const shortSha = commit.sha.slice(0, 7);
  return (
    <div className="flex w-full items-start gap-3 rounded-md bg-primary-foreground p-4">
      <GitCommitVertical className="size-6 shrink-0" />
      <p className="flex-1 text-base leading-6 text-foreground">{firstLine}</p>
      <a
        href={commit.html_url}
        target="_blank"
        rel="noreferrer"
        className="text-base leading-6 hover:underline"
      >
        {shortSha}
      </a>
    </div>
  );
}
