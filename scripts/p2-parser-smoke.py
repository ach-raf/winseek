"""P2 smoke test: Infinity-Parser2-Flash on llama-server (doc image -> layout JSON).

Renders a synthetic document page (or uses --image), asks for layout JSON with the
model card's layout prompt, checks the known strings come back, reports speed.

Usage (server running with -m Infinity-Parser2-Flash.*.gguf --mmproj ... ):
  python scripts/p2-parser-smoke.py --url http://127.0.0.1:8091 [--image page.png]
"""
import argparse, base64, io, json, sys, time, urllib.request

PROMPT = (
    "Extract layout information from the provided PDF image. For each layout element, "
    "output its bbox, category, and the text content within the bbox. "
    "Bbox format: [x1, y1, x2, y2]. Layout categories: ['header', 'title', 'text', 'figure', "
    "'table', 'formula', 'figure_caption', 'table_caption', 'formula_caption', 'figure_footnote', "
    "'table_footnote', 'page_footnote', 'footer']. Text extraction and formatting rules: "
    "For figure, the text must be empty. For formula, format as LaTeX. For table, format as HTML. "
    "For all other categories, format as Markdown. The output text must be exactly the original "
    "text from the image, with no translation or rewriting. Sort all layout elements in human "
    "reading order. Final output must be a single JSON object."
)
EXPECT = ["Quarterly Invoice 4471", "Winseek Hardware Ltd", "Graphics card", "1,249.00", "Total due"]


def synth_page():
    from PIL import Image, ImageDraw, ImageFont
    W, H = 1240, 1754
    im = Image.new("RGB", (W, H), "white")
    d = ImageDraw.Draw(im)

    def font(sz):
        for f in ("arial.ttf", "segoeui.ttf", "DejaVuSans.ttf"):
            try:
                return ImageFont.truetype(f, sz)
            except OSError:
                pass
        return ImageFont.load_default()

    d.text((100, 100), "Quarterly Invoice 4471", font=font(56), fill="black")
    d.text((100, 200), "Billed by Winseek Hardware Ltd, 12 Rue des Fleurs, Casablanca.", font=font(28), fill="black")
    d.text((100, 245), "Payment is due within 30 days of the invoice date.", font=font(28), fill="black")
    rows = [("Item", "Qty", "Price"), ("Graphics card", "1", "1,249.00"), ("NVMe drive 2TB", "2", "310.00"),
            ("Total due", "", "1,869.00")]
    y = 340
    for r in rows:
        for x0, cell in zip((100, 700, 900), r):
            d.rectangle([x0, y, x0 + (600 if x0 == 100 else 200), y + 60], outline="black", width=2)
            d.text((x0 + 12, y + 14), cell, font=font(28), fill="black")
        y += 60
    d.text((100, H - 120), "Page 1 of 1", font=font(24), fill="black")
    buf = io.BytesIO()
    im.save(buf, "PNG")
    return buf.getvalue(), "image/png"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="http://127.0.0.1:8091")
    ap.add_argument("--key")
    ap.add_argument("--image")
    a = ap.parse_args()

    if a.image:
        data, mime = open(a.image, "rb").read(), "image/png" if a.image.lower().endswith(".png") else "image/jpeg"
    else:
        data, mime = synth_page()
    body = {
        "messages": [{"role": "user", "content": [
            {"type": "image_url", "image_url": {"url": f"data:{mime};base64,{base64.b64encode(data).decode()}"}},
            {"type": "text", "text": PROMPT}]}],
        "temperature": 0, "top_p": 1, "max_tokens": 8192,
        "chat_template_kwargs": {"enable_thinking": False},
    }
    req = urllib.request.Request(f"{a.url}/v1/chat/completions", json.dumps(body).encode(),
                                 {"Content-Type": "application/json"})
    if a.key:
        req.add_header("Authorization", f"Bearer {a.key}")
    t = time.time()
    r = json.loads(urllib.request.urlopen(req, timeout=900).read())
    dt = time.time() - t
    out = r["choices"][0]["message"]["content"]
    u = r.get("usage", {})
    print(out[:3000])
    print(f"\n  {dt:.1f}s  prompt_tokens={u.get('prompt_tokens')} completion_tokens={u.get('completion_tokens')}"
          f"  ({(u.get('completion_tokens') or 0)/dt:.1f} tok/s overall)")
    fails = 0
    if not a.image:
        for s in EXPECT:
            ok = s in out
            fails += not ok
            print(f"  [{'PASS' if ok else 'FAIL'}] contains {s!r}")
    try:
        txt = out.strip().removeprefix("```json").removeprefix("```").removesuffix("```")
        json.loads(txt)
        print("  [PASS] output parses as JSON")
    except Exception as e:
        fails += 1
        print(f"  [FAIL] output parses as JSON ({e})")
    print(f"\n{'PARSER SMOKE PASS' if not fails else f'PARSER SMOKE FAIL ({fails})'}")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
