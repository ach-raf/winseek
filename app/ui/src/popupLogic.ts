/** The one viewport clamp shared by every HTML-drawn floating menu (launcher ctx menu, the
 *  filter row's chip dropdowns) so none of them can clip at the window edge or drift apart.
 *  Windows-menu behavior: keep the anchor point while the whole rect fits, otherwise slide the
 *  rect fully inside with `margin` breathing room (a menu taller/wider than the window pins to
 *  the margin and relies on its own max-height + scroll). */
export function fitMenuRect(x: number, y: number, w: number, h: number, vw: number, vh: number, margin = 8): { left: number; top: number } {
  const left = x + w > vw - margin ? Math.max(margin, vw - margin - w) : x;
  const top = y + h > vh - margin ? Math.max(margin, vh - margin - h) : y;
  return { left, top };
}
