import { Component, type ReactNode } from "react";

/** Renders a visible error card when the tree below throws. Without it an uncaught render error
 *  unmounts the whole app, and since the window body is transparent (the popup's rounded corners),
 *  the OS window would stay visible but paint nothing — an invisible trap the hotkey cannot fix
 *  (toggle only hides it again). A crash must always leave something on screen with a way out. */
export class ErrorBoundary extends Component<{ children: ReactNode }, { err: Error | null }> {
  state = { err: null as Error | null };
  static getDerivedStateFromError(err: Error) {
    return { err };
  }
  componentDidCatch(err: Error) {
    console.error("render crashed:", err);
  }
  render() {
    const { err } = this.state;
    if (!err) return this.props.children;
    return (
      <div style={{ height: "100%", display: "grid", placeItems: "center", padding: 24 }}>
        <div role="alert" style={{
          background: "var(--bg)", color: "var(--fg)", border: "1px solid var(--line)",
          borderRadius: 14, boxShadow: "var(--shadow)", padding: "22px 26px", maxWidth: 420,
          display: "flex", flexDirection: "column", gap: 10, alignItems: "flex-start",
        }}>
          <b style={{ fontSize: 16 }}>winseek hit a rendering error</b>
          <code style={{ font: "500 12px/1.5 var(--mono)", color: "var(--fg2)", wordBreak: "break-word" }}>{err.message}</code>
          <button onClick={() => location.reload() } style={{
            background: "var(--acc)", color: "var(--acc-ink)", fontWeight: 600, fontSize: 13.5,
            padding: "7px 18px", borderRadius: 8,
          }}>Reload winseek</button>
        </div>
      </div>
    );
  }
}
