import type { Plugin } from "@opencode-ai/plugin";
import { execFileSync } from "child_process";

const BINARY = "{{BINARY_PATH}}";
const FILE_TOOLS = ["edit", "write", "patch", "multiedit"];

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

  "tool.execute.before": async (inp, _out) => {
    if (!FILE_TOOLS.includes(inp.tool.toLowerCase())) return;
    fire("tool.execute.before", {
      session_id: inp.sessionID,
      tool: inp.tool,
      cwd: input.worktree,
    });
  },

  "tool.execute.after": async (inp, _out) => {
    if (!FILE_TOOLS.includes(inp.tool.toLowerCase())) return;
    fire("tool.execute.after", {
      session_id: inp.sessionID,
      tool: inp.tool,
      cwd: input.worktree,
    });
  },
});

export default plugin;
