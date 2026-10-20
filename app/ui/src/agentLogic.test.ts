import assert from "node:assert/strict";
import { test } from "node:test";
import { argsPreview, snippets } from "./agentLogic.ts";

test("snippets escape windows paths in JSON but not in shell / toml", () => {
  const s = snippets("C:\\Program Files\\winseek\\winseek-mcp.exe");
  // JSON gets doubled backslashes via stringify
  assert.equal(s.desktop, JSON.stringify({ mcpServers: { winseek: { command: "C:\\Program Files\\winseek\\winseek-mcp.exe" } } }, null, 2));
  assert.ok(s.desktop.includes("\\\\Program Files\\\\"));
  assert.ok(s.vscode.includes('"type": "stdio"'));
  // Claude Code: a single quoted command line
  assert.equal(s.claudeCode, 'claude mcp add winseek -- "C:\\Program Files\\winseek\\winseek-mcp.exe"');
  // Codex TOML: single quotes, one line of command
  assert.equal(s.codex, "[mcp_servers.winseek]\ncommand = 'C:\\Program Files\\winseek\\winseek-mcp.exe'");
});

test("snippets with a space-free path round-trip through JSON", () => {
  const s = snippets("C:\\dev\\winseek-mcp.exe");
  const parsed = JSON.parse(s.desktop) as { mcpServers: { winseek: { command: string } } };
  assert.equal(parsed.mcpServers.winseek.command, "C:\\dev\\winseek-mcp.exe");
});

test("argsPreview truncates and survives junk", () => {
  assert.equal(argsPreview(null), "");
  assert.equal(argsPreview("not an object"), "");
  assert.equal(argsPreview({ query: "invoice" }), '{"query":"invoice"}');
  const long = argsPreview({ query: "x".repeat(100) });
  assert.ok(long.endsWith("…"));
  assert.ok(long.length < 70);
});
