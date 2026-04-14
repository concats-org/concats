import { useEffect, useState } from "react";
import { useNavigate, useParams } from "react-router";
import { ArrowRight } from "lucide-react";
import { parsePullRequestLocation } from "@/pages/home-page";
import { Logo } from "@/components/brand";

export function Header() {
  const navigate = useNavigate();
  const { owner, repo, pullNumber } = useParams();
  const initial =
    owner && repo && pullNumber
      ? `https://github.com/${owner}/${repo}/pull/${pullNumber}`
      : "";

  const [value, setValue] = useState(initial);

  useEffect(() => {
    setValue(initial);
  }, [initial]);

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    const location = parsePullRequestLocation(value);
    if (!location) return;
    navigate(
      `/github/${location.owner}/${location.repo}/pull/${location.pullNumber}`,
    );
  };

  return (
    <div className="flex w-full flex-col items-stretch gap-4 sm:flex-row sm:items-center sm:gap-6">
      <Logo className="shrink-0 self-start text-foreground sm:self-auto" />
      <form
        onSubmit={handleSubmit}
        className="flex h-9 flex-1 items-stretch"
      >
        <input
          type="text"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          placeholder="https://github.com/owner/repo/pull/123"
          className="min-w-0 flex-1 rounded-l-md border border-r-0 border-primary bg-background px-3 text-base outline-none placeholder:text-muted-foreground focus:ring-2 focus:ring-primary/20"
        />
        <button
          type="submit"
          aria-label="Open PR"
          className="flex w-9 shrink-0 items-center justify-center rounded-r-md border border-l-0 border-primary bg-primary text-primary-foreground hover:bg-primary/90"
        >
          <ArrowRight className="size-4" />
        </button>
      </form>
    </div>
  );
}
