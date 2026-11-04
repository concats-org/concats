import type { Plugin } from "@opencode-ai/plugin";
import { execFileSync } from "child_process";

const BINARY = "{{BINARY_PATH}}";

function fire(event: string, payload: Record<string, unknown>) {
  try {
    execFileSync(BINARY, ["hook", "opencode", event], {
      input: JSON.stringify(payload),
    });
  } catch {
    // Best-effort: do not block the agent on hook failure.
  }
}

const plugin: Plugin = async (input) => ({
  async event({ event }) {
    switch (event.type) {
      case "session.created":
        fire("session.created", {
          session_id: event.properties.info.id,
          cwd: input.worktree,
        });
        break;
      case "session.idle":
        fire("session.idle", {
          session_id: event.properties.sessionID,
          cwd: input.worktree,
        });
        break;
    }
  },
});

export default plugin;
