import { execFileSync } from "child_process";

const BINARY = "{{BINARY_PATH}}";
const FILE_TOOLS = ["write", "edit", "create", "patch", "apply_patch"];

function fire(event: string, payload: Record<string, unknown>) {
  try {
    execFileSync(BINARY, ["hook", "amp", event], {
      input: JSON.stringify(payload),
    });
  } catch {
    // Best-effort: do not block the agent on hook failure.
  }
}

export default {
  name: "concats",

  "session.start"() {
    fire("session.start", { cwd: process.cwd() });
  },

  "agent.start"(event: { message: string; id: number }) {
    fire("agent.start", {
      prompt: event.message,
      cwd: process.cwd(),
    });
  },

  "agent.end"(event: {
    message: string;
    id: number;
    status: string;
    messages: unknown[];
  }) {
    fire("agent.end", {
      status: event.status,
      cwd: process.cwd(),
    });
  },

  onToolCall(event: { sessionId: string; tool: string; input: unknown }) {
    if (!FILE_TOOLS.includes(event.tool.toLowerCase())) return;
    fire("tool.call", {
      session_id: event.sessionId,
      tool: event.tool,
      cwd: process.cwd(),
    });
  },

  onToolResult(event: {
    sessionId: string;
    tool: string;
    output: unknown;
  }) {
    if (!FILE_TOOLS.includes(event.tool.toLowerCase())) return;
    fire("tool.result", {
      session_id: event.sessionId,
      tool: event.tool,
      cwd: process.cwd(),
    });
  },
};
