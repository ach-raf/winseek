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

/** What the PATH row says under the switch. The installer manages the machine PATH; this row
 *  manages the user PATH, so a machine entry wins and there is nothing left to do here. */
export function pathNote(s: { user_path: boolean; machine_path: boolean }): string {
  if (s.machine_path && s.user_path) return "On PATH for all users (the installer added it), and in your user PATH.";
  if (s.machine_path) return "On PATH for all users — the installer added it.";
  if (s.user_path) return "On your user PATH. New terminals will find winseek-cli and winseek-mcp.";
  return "Not on PATH. Turn this on to run winseek-cli from any terminal.";
}
