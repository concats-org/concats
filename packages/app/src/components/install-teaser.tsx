import { useState } from "react";
import { Check, Copy, TriangleAlert } from "lucide-react";

const INSTALL_CMD = "curl -LsSf https://concats.org/install.sh | sh";
const INIT_CMD = "concats init";

export function InstallTeaser() {
  return (
    <div className="flex w-full flex-col gap-3 rounded-md border-2 border-[#ffd100] p-4">
      <div className="flex w-full items-start gap-3">
        <TriangleAlert className="size-6 shrink-0" />
        <p className="flex-1 text-base leading-6 font-bold">
          No Sessions Found
        </p>
      </div>
      <p className="text-base leading-6">
        No sessions have been captured or pushed for this pull request’s
        commits.
      </p>
      <p className="text-base leading-6">
        Install concats using the standalone installer script to capture
        sessions:
      </p>
      <CodeBlock command={INSTALL_CMD} />
      <p className="text-base leading-6">
        Once installed initialize concats and follow the instructions
      </p>
      <CodeBlock command={INIT_CMD} />
      <p className="text-base leading-6">
        With that you’re all set to capture and push sessions.
      </p>
    </div>
  );
}

function CodeBlock({ command }: { command: string }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // clipboard unavailable — leave state untouched
    }
  };

  return (
    <div className="flex w-full items-center gap-2.5 rounded-md bg-primary-foreground p-3">
      <code className="min-w-0 flex-1 overflow-x-auto text-base leading-6 whitespace-pre">
        {command}
      </code>
      <button
        type="button"
        onClick={handleCopy}
        aria-label="Copy command"
        className="shrink-0 text-foreground hover:text-muted-foreground"
      >
        {copied ? <Check className="size-6" /> : <Copy className="size-6" />}
      </button>
    </div>
  );
}
