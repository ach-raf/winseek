"""P2 smoke test: EmbeddingGemma-2 on llama-server (text + images).

Checks:
  1. vision is loaded (/props)
  2. different images -> different vectors
  3. parity vs a torch reference index (photo app emb.npz + chunks.json), if given
  4. text -> image retrieval agrees with the torch reference ranking
  5. throughput (images/s, texts/s)

Usage (server already running with --embeddings --pooling mean --mmproj ...):
  python scripts/p2-smoke.py --url http://127.0.0.1:8090 [--ref-dir <photo app index_store>] [-n 50]
"""
import argparse, base64, json, mimetypes, random, sys, time, urllib.request
import numpy as np

QUERIES = ["a dog", "food on a plate", "a screenshot of a phone", "people at the beach",
           "a document with text", "a car", "a birthday cake", "night city lights"]


def post(url, body, key=None):
    req = urllib.request.Request(url, json.dumps(body).encode(), {"Content-Type": "application/json"})
    if key:
        req.add_header("Authorization", f"Bearer {key}")
    with urllib.request.urlopen(req, timeout=600) as r:
        return json.loads(r.read())


def norm(v):
    v = np.asarray(v, dtype=np.float32)
    return v / np.linalg.norm(v, axis=-1, keepdims=True)


def embed_images(base, paths, key):
    out = []
    for p in paths:
        mime = mimetypes.guess_type(p)[0] or "image/jpeg"
        b64 = base64.b64encode(open(p, "rb").read()).decode()
        part = {"type": "image_url", "image_url": {"url": f"data:{mime};base64,{b64}"}}
        r = post(f"{base}/v1/embeddings", {"input": [{"content": [part]}]}, key)
        out.append(r["data"][0]["embedding"])
    return norm(out)


def embed_texts(base, texts, key):
    r = post(f"{base}/v1/embeddings", {"input": texts}, key)
    return norm([d["embedding"] for d in sorted(r["data"], key=lambda d: d["index"])])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="http://127.0.0.1:8090")
    ap.add_argument("--key")
    ap.add_argument("--ref-dir", help="photo app index_store (emb.npz + chunks.json)")
    ap.add_argument("--images", nargs="*", default=[])
    ap.add_argument("-n", type=int, default=50)
    a = ap.parse_args()
    fails = 0

    def check(name, ok, detail=""):
        nonlocal fails
        fails += not ok
        print(f"  [{'PASS' if ok else 'FAIL'}] {name} {detail}")

    try:
        props = json.loads(urllib.request.urlopen(f"{a.url}/props", timeout=10).read())
        mods = props.get("modalities", {})
        check("vision loaded (/props)", bool(mods.get("vision")), json.dumps(mods))
    except Exception as e:
        check("vision loaded (/props)", False, repr(e))

    ref = None
    paths = list(a.images)
    if a.ref_dir:
        emb = np.load(f"{a.ref_dir}/emb.npz")["emb"]
        chunks = json.load(open(f"{a.ref_dir}/chunks.json", encoding="utf-8"))
        idx = [i for i, c in enumerate(chunks) if c.get("kind") == "image"]
        random.Random(0).shuffle(idx)
        idx = idx[: a.n]
        paths = [chunks[i]["path"] for i in idx]
        ref = norm(emb[idx])
    if len(paths) < 2:
        sys.exit("need --ref-dir or at least 2 --images")

    t = time.time()
    img = embed_images(a.url, paths, a.key)
    dt = time.time() - t
    print(f"  dim={img.shape[1]}  {len(paths)} images in {dt:.1f}s ({len(paths)/dt:.1f} img/s)")

    sim = img @ img.T
    off = sim[~np.eye(len(img), dtype=bool)]
    check("different images -> different vectors", off.max() < 0.999,
          f"max off-diag cos={off.max():.4f} mean={off.mean():.3f}")

    t = time.time()
    q = embed_texts(a.url, [f"task: search result | query: {x}" for x in QUERIES], a.key)
    print(f"  {len(QUERIES)} queries in {time.time()-t:.2f}s")

    if ref is not None:
        d = min(ref.shape[1], img.shape[1])
        cos = np.sum(norm(img[:, :d]) * norm(ref[:, :d]), axis=1)
        check("image parity vs torch (mean cos >= 0.97)", cos.mean() >= 0.97,
              f"mean={cos.mean():.4f} min={cos.min():.4f}")
        qd = norm(q[:, :d])
        overlaps = []
        for qi in range(len(QUERIES)):
            a5 = set(np.argsort(-(norm(img[:, :d]) @ qd[qi]))[:5])
            b5 = set(np.argsort(-(ref[:, :d] @ qd[qi]))[:5])
            overlaps.append(len(a5 & b5) / 5)
        check("text->image top-5 agrees with torch (mean overlap >= 0.6)",
              np.mean(overlaps) >= 0.6, f"mean={np.mean(overlaps):.2f} per-query={overlaps}")
    else:
        top = (q @ img.T).max(axis=1)
        print(f"  best text->image cos per query: {np.round(top, 3).tolist()}")

    print(f"\n{'P2 SMOKE PASS' if not fails else f'P2 SMOKE FAIL ({fails})'}")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
