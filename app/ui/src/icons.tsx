// Shared v4 icon set. Size via the `size` prop or CSS.
import type { ReactNode } from "react";

type P = { size?: number; className?: string };
const S = ({ children, size, w = 1.8, className }: { children: ReactNode; size?: number; w?: number; className?: string }) => (
  <svg viewBox="0 0 24 24" width={size} height={size} fill="none" stroke="currentColor" strokeWidth={w} strokeLinecap="round" strokeLinejoin="round" className={className} aria-hidden>
    {children}
  </svg>
);

export const IMag = ({ size = 20, className = "mag" }: P) => <S size={size} w={2.2} className={className}><circle cx="10.5" cy="10.5" r="6.5" /><path d="M15.5 15.5 21 21" /></S>;
export const ISearch = ({ size, className }: P) => <S size={size} w={2} className={className}><circle cx="10.5" cy="10.5" r="6.5" /><path d="M15.5 15.5 21 21" /></S>;
export const IX = ({ size = 11, className }: P) => <S size={size} w={2.6} className={className}><path d="M6 6l12 12M18 6 6 18" /></S>;
export const IExpand = ({ size = 17, className }: P) => <S size={size} className={className}><path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" /></S>;
/** Corners pointing inward: "collapse into the popup" — the counterpart of IExpand, so the full
 *  window's back-to-popup button never reads as maximize (IMax is a plain square: wrong verb). */
export const IContract = ({ size = 17, className }: P) => <S size={size} className={className}><path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5" /></S>;
export const ITime = ({ size, className }: P) => <S size={size} className={className}><rect x="3.5" y="4.5" width="17" height="15" rx="3" /><path d="m3.5 15 4.5-4 4 3.5 3-2.5 5.5 4.5" /><circle cx="15.5" cy="9" r="1.2" /></S>;
export const IColl = ({ size, className }: P) => <S size={size} className={className}><rect x="3.5" y="3.5" width="7" height="7" rx="2" /><rect x="13.5" y="3.5" width="7" height="7" rx="2" /><rect x="3.5" y="13.5" width="7" height="7" rx="2" /><rect x="13.5" y="13.5" width="7" height="7" rx="2" /></S>;
export const IClean = ({ size, className }: P) => <S size={size} className={className}><path d="M4 7h16M9 7V4.5h6V7M6.5 7l1 12.5h9l1-12.5M10 11v5M14 11v5" /></S>;
// A toothed cog: the spoke-and-circle glyph reads as a brightness/sun button in the popup bar.
export const IGear = ({ size, className }: P) => <S size={size} className={className}><path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.24a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.24a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.24a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.24a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" /><circle cx="12" cy="12" r="3" /></S>;
export const ISet = ({ size, className }: P) => <S size={size} w={2.2} className={className}><path d="M5 8h9M18 8h1M5 16h1M10 16h9" /><circle cx="16" cy="8" r="2" /><circle cx="8" cy="16" r="2" /></S>;
export const ICmd = ({ size, className }: P) => <S size={size} w={2.4} className={className}><path d="m5 8 5 4-5 4M13 17h6" /></S>;
export const ICheck = ({ size = 12, className }: P) => <S size={size} w={3.6} className={className}><path d="m5 12 5 5 9-10" /></S>;
export const IPlus = ({ size, className }: P) => <S size={size} w={2} className={className}><path d="M12 5v14M5 12h14" /></S>;
export const IPlay = ({ size, className }: P) => <svg viewBox="0 0 24 24" width={size} height={size} fill="currentColor" className={className} aria-hidden><path d="M8 5v14l11-7z" /></svg>;
export const ILogo = ({ size = 15 }: P) => <svg viewBox="0 0 16 16" width={size} height={size} fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" aria-hidden><circle cx="7" cy="7" r="4.2" /><path d="m10.4 10.4 3.2 3.2" /></svg>;
export const IMin = () => <svg width="11" height="11" viewBox="0 0 11 11" stroke="currentColor" fill="none" aria-hidden><path d="M1 5.5h9" /></svg>;
export const IMax = () => <svg width="11" height="11" viewBox="0 0 11 11" stroke="currentColor" fill="none" aria-hidden><rect x="1.5" y="1.5" width="8" height="8" /></svg>;
export const ICls = () => <svg width="11" height="11" viewBox="0 0 11 11" stroke="currentColor" fill="none" aria-hidden><path d="M1 1l9 9M10 1 1 10" /></svg>;
export const IChevL = ({ size = 15, className }: P) => <S size={size} w={2.2} className={className}><path d="m15 18-6-6 6-6" /></S>;
export const IUp = ({ size = 15, className }: P) => <S size={size} w={2.2} className={className}><path d="m6 14 6-6 6 6" /><path d="M12 8v12" /></S>;
export const IRefresh = ({ size = 15, className }: P) => <S size={size} w={2} className={className}><path d="M20 12a8 8 0 1 1-2.6-5.9" /><path d="M20 4v5h-5" /></S>;
export const IFolder = () => (
  <svg viewBox="0 0 32 32" aria-hidden>
    <path d="M3 9a3 3 0 0 1 3-3h6.2a2 2 0 0 1 1.5.7L15.5 9H26a3 3 0 0 1 3 3v2H3z" fill="#e3a21a" />
    <path d="M3 12.5a2.5 2.5 0 0 1 2.5-2.5h21a2.5 2.5 0 0 1 2.5 2.5V23a3 3 0 0 1-3 3H6a3 3 0 0 1-3-3z" fill="#ffca4d" />
  </svg>
);
/** v4 file glyph: paper sheet with a coloured extension badge. `col` = badge colour, `lab` = up to 4 letters. */
export function FileGlyph({ col, lab }: { col: string; lab: string }) {
  const wide = lab.length > 3;
  return (
    <svg viewBox="0 0 32 32" aria-hidden>
      <path d="M9 3.5h10.2L26 10.3V26a2.5 2.5 0 0 1-2.5 2.5h-14A2.5 2.5 0 0 1 7 26V6a2.5 2.5 0 0 1 2-2.5z" fill="var(--paper)" stroke="var(--paper-line)" />
      <path d="M19.2 3.5v4.3a2.5 2.5 0 0 0 2.5 2.5H26z" fill="var(--fold)" />
      <rect x="3" y="15.5" width={wide ? 21 : 18} height="9.5" rx="2.4" fill={col} />
      <text x={wide ? 13.5 : 12} y="22.6" textAnchor="middle" fontSize="6.2" fontWeight="800" fill="#fff" fontFamily="Figtree Variable, Figtree, sans-serif">{lab}</text>
    </svg>
  );
}
