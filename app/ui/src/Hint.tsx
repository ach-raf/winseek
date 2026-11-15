import { useId, type ReactNode } from "react";

/** Rich hover tooltip for the few places where the hover text carries real content (the match
 *  reason on a row badge). Uses the interest-target + hint-popover pair with CSS anchor
 *  positioning when the webview has all three, so the text is hover- AND keyboard-reachable and
 *  flips at the viewport edge; anything older keeps the plain `title` attribute (CLAUDE.md:
 *  WebView2 evergreen, no polyfills for missing pieces). */
export function Hint({ text, className, children }: { text?: string; className?: string; children: ReactNode }) {
  const id = useId();
  const modern = typeof HTMLElement !== "undefined" && typeof CSS !== "undefined"
    && "popover" in HTMLElement.prototype
    && "interestfor" in HTMLElement.prototype
    && CSS.supports("anchor-name: --a");
  if (!modern || !text) return <span className={className} title={text}>{children}</span>;
  return (
    <>
      <span className={className} aria-describedby={id} {...({ interestfor: id, popovertarget: id } as Record<string, string>)}>{children}</span>
      <div id={id} className="hint-pop" popover="hint" role="tooltip">{text}</div>
    </>
  );
}
