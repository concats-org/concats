import { describe, expect, it } from "bun:test";
import { parseTurn, parseSnapshot, suggestSubject } from "./index";

describe("parseTurn", () => {
  it("parses a minimal turn with a prompt and response", () => {
    const input = [
      "turn",
      "",
      "<prompt>hello</prompt>",
      "<response>world</response>",
      "",
      "Session: sess-a",
      "Agent: gpt",
    ].join("\n");

    const turn = parseTurn(input);
    expect(turn).not.toBeNull();
    expect(turn).toEqual({
      subject: "turn",
      sessionId: "sess-a",
      agentName: "gpt",
      entries: [
        { kind: "prompt", text: "hello" },
        { kind: "response", text: "world" },
      ],
    });
  });

  it("parses tool calls with toolKind", () => {
    const input = [
      "turn",
      "",
      '<tool kind="read"/>',
      "",
      "Session: sess-a",
    ].join("\n");

    const turn = parseTurn(input);
    expect(turn?.entries).toEqual([{ kind: "tool_call", toolKind: "read" }]);
    expect(turn?.agentName).toBeNull();
  });

  it("returns null for unparseable input", () => {
    expect(parseTurn("not a turn message")).toBeNull();
  });
});

describe("parseSnapshot", () => {
  it("parses a snapshot with a reason", () => {
    const input = "snapshot\n\nSession: sess-a\nReason: turn_commit";
    expect(parseSnapshot(input)).toEqual({
      sessionId: "sess-a",
      reason: "turn_commit",
    });
  });

  it("parses a snapshot without a reason", () => {
    const input = "snapshot\n\nSession: sess-a";
    expect(parseSnapshot(input)).toEqual({
      sessionId: "sess-a",
      reason: null,
    });
  });

  it("returns null when the Session trailer is missing", () => {
    expect(parseSnapshot("snapshot\n\nReason: turn_commit")).toBeNull();
  });
});

describe("suggestSubject", () => {
  it("summarizes the first prompt entry", () => {
    const input = [
      "turn",
      "",
      "<prompt>Refactor the session storage</prompt>",
      "",
      "Session: sess-a",
    ].join("\n");

    expect(suggestSubject(input)).toBe("Refactor the session storage");
  });

  it("returns null for unparseable input", () => {
    expect(suggestSubject("garbage")).toBeNull();
  });
});
