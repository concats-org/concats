import { Github } from "lucide-react";
import { Skeleton } from "@/components/ui/skeleton";
import type { PullData } from "@/hooks/use-pull-data";

interface PullSummaryProps {
  pull: PullData | null;
  loading: boolean;
}

type PullState = "draft" | "open" | "merged" | "closed";

function resolveState(pull: PullData): PullState {
  if (pull.merged_at) return "merged";
  if (pull.state === "closed") return "closed";
  if (pull.draft) return "draft";
  return "open";
}

const STATE_STYLES: Record<PullState, { label: string; className: string }> = {
  draft: { label: "Draft", className: "bg-[#8d8d8d] text-white" },
  open: { label: "Open", className: "bg-[#2b9a66] text-white" },
  merged: { label: "Merged", className: "bg-[#6e56cf] text-white" },
  closed: { label: "Closed", className: "bg-[#e5484d] text-white" },
};

function mergeCopy(state: PullState, commitCount: number): string {
  const noun = commitCount === 1 ? "commit" : "commits";
  const verb =
    state === "merged"
      ? "merged"
      : state === "closed"
        ? "wanted to merge"
        : "wants to merge";
  return `${verb} ${commitCount} ${noun} into`;
}

export function PullSummary({ pull, loading }: PullSummaryProps) {
  if (loading) {
    return (
      <div className="w-full rounded-md border border-border bg-background p-4 shadow-xs">
        <Skeleton className="h-7 w-3/4" />
        <Skeleton className="mt-4 h-4 w-full" />
        <Skeleton className="mt-2 h-4 w-2/3" />
      </div>
    );
  }

  if (!pull) return null;

  const state = resolveState(pull);
  const stateStyle = STATE_STYLES[state];
  const author = pull.user?.login ?? "unknown";
  const authorUrl = pull.user?.html_url ?? "#";
  const baseRef = pull.base.ref;
  const baseUrl = `${pull.base.repo.html_url}/tree/${baseRef}`;
  const headRef = pull.head.ref;
  const headUrl = pull.head.repo
    ? `${pull.head.repo.html_url}/tree/${headRef}`
    : null;

  return (
    <div className="flex w-full flex-col gap-3 rounded-md border border-border bg-background p-4 shadow-xs">
      <h1 className="text-xl leading-7 font-bold">{pull.title}</h1>
      <hr className="border-border" />
      {pull.body && (
        <p className="text-base leading-6 whitespace-pre-wrap">{pull.body}</p>
      )}
      <hr className="border-border" />
      <div className="flex flex-wrap items-center gap-3 text-xs leading-4">
        <a
          href={pull.html_url}
          target="_blank"
          rel="noreferrer"
          aria-label="Open PR on GitHub"
          className="shrink-0 hover:text-muted-foreground"
        >
          <Github className="size-6" />
        </a>
        <span
          className={`inline-flex h-[22px] items-center rounded-md px-2.5 text-xs font-medium ${stateStyle.className}`}
        >
          {stateStyle.label}
        </span>
        <a
          href={authorUrl}
          target="_blank"
          rel="noreferrer"
          className="underline"
        >
          {author}
        </a>
        <span>{mergeCopy(state, pull.commits)}</span>
        <BranchBadge href={baseUrl} label={baseRef} emphasized />
        <span>from</span>
        <BranchBadge href={headUrl} label={headRef} />
      </div>
    </div>
  );
}

function BranchBadge({
  href,
  label,
  emphasized = false,
}: {
  href: string | null;
  label: string;
  emphasized?: boolean;
}) {
  const className = `inline-flex h-[22px] items-center rounded-md bg-secondary px-2.5 text-xs ${
    emphasized ? "font-medium text-primary" : "text-foreground"
  }`;
  if (!href) return <span className={className}>{label}</span>;
  return (
    <a href={href} target="_blank" rel="noreferrer" className={className}>
      {label}
    </a>
  );
}
