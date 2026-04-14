import { useState } from "react";
import { useNavigate } from "react-router";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

interface PullLocation {
  owner: string;
  repo: string;
  pullNumber: string;
}

const GITHUB_URL_PATTERN = /github\.com\/([^/]+)\/([^/]+)\/pull\/(\d+)/;
const PATH_PATTERN = /^\/?([^/]+)\/([^/]+)\/pull\/(\d+)\/?$/;
const URL_ERROR =
  "Enter a GitHub PR URL or path like owner/repo/pull/123";

export function parsePullRequestLocation(input: string): PullLocation | null {
  const match = input.match(GITHUB_URL_PATTERN) ?? input.match(PATH_PATTERN);
  if (!match) return null;
  return { owner: match[1], repo: match[2], pullNumber: match[3] };
}

export function HomePage() {
  const navigate = useNavigate();
  const [url, setUrl] = useState("");
  const [error, setError] = useState<string | null>(null);

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    const location = parsePullRequestLocation(url);
    if (!location) {
      setError(URL_ERROR);
      return;
    }
    setError(null);
    navigate(`/github/${location.owner}/${location.repo}/pull/${location.pullNumber}`);
  };

  return (
    <div className="flex min-h-screen items-center justify-center bg-background px-4">
      <Card className="w-full max-w-md">
        <HomeHeader />
        <CardContent>
          <form onSubmit={handleSubmit} className="space-y-4">
            <Input
              type="text"
              placeholder="https://github.com/owner/repo/pull/123"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
            />
            {error && <p className="text-xs text-destructive">{error}</p>}
            <Button type="submit" className="w-full">
              Open PR
            </Button>
          </form>
        </CardContent>
      </Card>
    </div>
  );
}

function HomeHeader() {
  return (
    <CardHeader className="text-center">
      <CardTitle className="text-2xl">
        <span className="text-yellow-500">+</span>
        <span className="text-red-500">+</span>
        <span className="text-green-600">+</span>
        <span className="ml-1">concat.me</span>
      </CardTitle>
      <p className="text-sm text-muted-foreground">
        View pull requests with agent session history
      </p>
    </CardHeader>
  );
}
