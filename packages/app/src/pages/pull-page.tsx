import { useParams } from "react-router";
import { Header } from "@/components/header";
import { Footer } from "@/components/footer";
import { PullSummary } from "@/components/pull-summary";
import { CommitTimeline } from "@/components/commit-timeline";
import { InstallTeaser } from "@/components/install-teaser";
import {
  usePull,
  usePullCommits,
  useSessionsAndMatches,
} from "@/hooks/use-pull-data";

export function PullPage() {
  const { owner, repo, pullNumber } = useParams();

  if (!owner || !repo || !pullNumber) {
    return (
      <div className="flex h-screen items-center justify-center">
        <p className="text-muted-foreground">
          Navigate to /github/:owner/:repo/pull/:number
        </p>
      </div>
    );
  }

  const num = parseInt(pullNumber, 10);

  return <PullPageInner owner={owner} repo={repo} pullNumber={num} />;
}

interface PullPageInnerProps {
  owner: string;
  repo: string;
  pullNumber: number;
}

function PullPageInner({ owner, repo, pullNumber }: PullPageInnerProps) {
  const pull = usePull(owner, repo, pullNumber);
  const commits = usePullCommits(owner, repo, pullNumber);
  const { sessions, turns, matches, loading: sessionsLoading } =
    useSessionsAndMatches(owner, repo, commits.data);

  return (
    <div className="flex min-h-screen flex-col items-center bg-background px-4 py-6 md:p-8">
      <div className="flex w-full max-w-[1000px] flex-col items-start gap-6">
        <Header />

        <hr className="w-full border-border" />

        {pull.error && (
          <div className="w-full rounded-md border border-destructive/50 bg-destructive/10 p-4 text-sm text-destructive">
            {pull.error}
          </div>
        )}

        <PullSummary pull={pull.data} loading={pull.loading} />

        {sessions.error && (
          <div className="w-full rounded-md border border-orange-300 bg-orange-50 p-4 text-sm text-orange-800">
            Sessions: {sessions.error}
          </div>
        )}

        {!sessionsLoading &&
          !commits.loading &&
          commits.data &&
          commits.data.length > 0 &&
          matches.length === 0 && <InstallTeaser />}

        <CommitTimeline
          commits={commits.data}
          matches={matches}
          sessions={sessions.data}
          turns={turns}
          loading={commits.loading}
          sessionsLoading={sessionsLoading}
        />

        <hr className="w-full border-border" />

        <Footer />
      </div>
    </div>
  );
}
