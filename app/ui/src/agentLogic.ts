/** Copy-ready connect snippets for the agent hosts (PRD A6). Pure: tested. */

export interface Snippets {
  /** `claude mcp add …` for a terminal. */
  claudeCode: string;
  /** `mcpServers` JSON for Claude Desktop / Cursor / Windsurf. */
  desktop: string;
  /** `.vscode/mcp.json` (`servers`, type stdio). */
  vscode: string;
  /** `~/.codex/config.toml` TOML. */
  codex: string;
}

/** `path` is the real winseek-mcp.exe location; JSON escaping is JSON.stringify's job
 *  (backslashes!). Codex takes single quotes so Windows paths need no doubling. */
export function snippets(path: string): Snippets {
  return {
    claudeCode: `claude mcp add winseek -- "${path}"`,
    desktop: JSON.stringify({ mcpServers: { winseek: { command: path } } }, null, 2),
    vscode: JSON.stringify({ servers: { winseek: { type: "stdio", command: path } } }, null, 2),
    codex: `[mcp_servers.winseek]\ncommand = '${path}'`,
  };
}

/** One-line preview of a log entry's arguments (activity rows). */
export function argsPreview(args: unknown): string {
  if (args == null || typeof args !== "object") return "";
  try {
    const s = JSON.stringify(args);
    return s.length > 64 ? `${s.slice(0, 63)}…` : s;
  } catch {
    return ""; // circular or otherwise unserializable: show nothing rather than crash the log
  }
}
