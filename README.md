# winseek

Instant file-name search plus search by meaning, for Windows. Everything-speed names, EmbeddingGemma semantics — everything on-device.

<!--
  TODO: drop a 15–20 s GIF of Alt+Space → type "invoice" → meaning results → Quick Look.
  Keep it under ~3 MB (convert to WebP or use gifski -q 80) and place it at docs/img/hero.gif.
  <p align="center"><img src="docs/img/hero.gif" alt="winseek in action" width="640"></p>
-->

One hotkey opens a Spotlight-style bar. Typing `report` finds every file with "report" in its name in milliseconds — every drive, kept live by the NTFS change journal, no re-scanning. Typing `a dog on the beach` or `invoice total` finds the photo or scanned document whose *contents* match, using embeddings that never leave your machine.

winseek is in early development (v0.1.0). It works end to end today — names across all volumes, live updates, image and document meaning search — but expect rough edges. Tested on Windows 10 x64 (19045) with an NVIDIA RTX 3060; the CPU and Vulkan paths are automatic fallbacks, not yet polished.

## What you can search

| | Examples |
|---|---|
| **File & folder names** | `invoice`, `report 2024`, `*.psd`, `ext:jpg;png beach`, `folder:downloads contract` — every file on every NTFS, ReFS, exFAT and FAT volume, including USB drives |
| **Images** | describe them: `sunset over the sea`, `a whiteboard with diagrams`, `a receipt on a table` |
| **Documents & PDFs** | their text, including scanned pages: `bank statement`, `contract notice period` |
| **Scans plain OCR gives up on** | a 2B vision-language model reads the page instead — it read an Arabic ID card in our testing |

Names cover every file on every volume. Meaning search covers the folders you choose (default: Pictures, Documents, Desktop, Downloads) — never all 12 million files.

## How it works

```
                    Alt+Space
                       │
        ┌──────────────▼───────────────┐         ┌──────────────────────────────┐
        │  winseek-app (Tauri, user)   │         │  searchd (Windows service)   │
        │                              │  named  │                              │
        │  popup · library · preview   │◄───────►│  Tier 0 only: raw $MFT scan, │
        │  Tier 1 indexing worker      │  pipe   │  USN journal, name index     │
        └───────┬──────────────┬───────┘         └──────────────────────────────┘
                │              │
                ▼              ▼
        ┌──────────────┐  ┌─────────────────────┐
        │ llama-server │  │     llama-server    │
        │ EmbeddingGemma│ │ Infinity-Parser2-   │
        │      -2      │  │        Flash        │
        └──────────────┘  └─────────────────────┘
          spawned on demand · localhost only · unloaded when idle
```

Three processes, two tiers:

- **`searchd`** is a small Windows service that owns the *name index*. It reads the NTFS master file table directly (the same approach as WizTree/Everything), watches the USN change journal so renames, creations and deletes apply within about a second, and persists a snapshot to disk so restarts are instant. Non-NTFS drives get a folder watcher; ReFS gets its own journal walker; drives can be ejected safely while indexed. It never reads file contents and never touches a model.
- **`winseek-app`** is a Tauri app running as *you* — the popup (Alt+Space), the photo library, the preview, and the Tier 1 worker that reads file contents and embeds them. Content work runs unprivileged on purpose: it automatically respects file ACLs, and no model code ever runs with SYSTEM rights.
- **`llama-server`** children carry the two models: EmbeddingGemma-2 for embeddings and Infinity-Parser2-Flash for scanned documents. They start only when there is work, bind to 127.0.0.1 with a per-launch API key, and are killed after going idle — so a search session costs you nothing when you're not indexing.

Everything lands in one SQLite store (FTS5 for text, a vector table for embeddings, USN cursors, settings) that repairs itself: if the file is ever damaged, winseek salvages what it can and re-derives the rest on its own.

## Name search — the Everything half

A full scan of C: — 5.95 million files and folders — takes about 33 seconds; the saved snapshot comes back in under 5. Updates arrive from the USN journal, not from re-scans, so the index stays live at negligible cost.

The query syntax:

| Query | Matches |
|---|---|
| `invoice report` | names containing both terms |
| `"exact phrase"` | the phrase, verbatim |
| `*.psd` | wildcard (`*`, `?`) against the whole name |
| `ext:jpg;png` | files with those extensions |
| `folder:`, `file:` | restrict to folders or files |
| `docs\2024` | a term containing `\` matches the path |

`size:` and date filters are on the roadmap.

## Search by meaning

Text and images are embedded with **EmbeddingGemma-2** (Q8_0 GGUF, ~310 MB + a 555 MB vision encoder) running on llama.cpp. Raw cosine similarity is meaningless on its own, so results pass through a calibrated relevance gate: on a 20-query evaluation, precision at the top of the list went from 0.22 to 0.70, and the number of relevant results shown nearly doubled. The gate's threshold and per-search hub vector are learned from your own index, per model.

Resource discipline is the other half of the design:

- **Searching never touches the GPU.** The query embedder runs on the CPU (~30 ms per query, ~300 MB RAM), so a game can't lose VRAM to a search.
- **Indexing peaks around 2 GB of VRAM.** The image encoder and the document parser never run at the same time — work is phased, and servers are unloaded after two idle minutes.
- **Profiles** (Low / Balanced / Fast) trade speed against memory; the parser uses 1–4 GPU slots depending on the profile, with pages batched concurrently.
- Models are downloaded on first run from pinned llama.cpp releases, SHA-256 verified and resumable, with automatic CUDA → Vulkan → CPU fallback if a build fails its health check.

## Document parsing

Windows OCR handles readable screenshots, but scanned pages need more. When extracted text looks sparse or junk-heavy (in any script — the check is language-agnostic), the page goes to **Infinity-Parser2-Flash**, a 2B vision model that outputs layout-aware Markdown with tables as HTML. It costs a separate ~1.5 GB download, is strictly optional, and runs at roughly 2–3 s per page on an RTX 3060. In the preview you can extract text, or copy a table as HTML or Markdown from any image or PDF page.

The parser is a small generative model: strong on documents, weak on complex charts and rotated tables. Pages it can't read confidently are skipped, not hallucinated into the index.

## Use winseek from AI agents

winseek ships a small MCP server (`winseek-mcp.exe`, strictly read-only) that any MCP client can use: name search over every drive with type/date/size filters, search by meaning, reading text out of PDFs and Office files, apps & Settings lookup, index status and the library queries. It never opens, launches, renames or deletes anything — agents get what winseek *knows*; actions stay with their own tools and with you.

Turn it on in winseek → Settings › Agents (off by default), then connect Claude Code:

```
claude mcp add winseek -- "C:\Program Files\winseek\winseek-mcp.exe"
```

Claude Desktop / Cursor / VS Code / Codex snippets are copy-ready in the same Settings section, and every agent call shows up in the activity log there.

## Privacy

Nothing leaves your machine. The index, the models and every query stay local; llama-server children bind to 127.0.0.1 with a random per-launch key; there is no telemetry, no account, and no network call except the one-time model download you trigger from Settings. The name service never reads file contents, and the content worker runs as your user — so files you can't read, winseek can't either.

## Requirements & install

- Windows 10 or 11, x64 (WebView2 is installed automatically if missing)
- Administrator approval once, for the `searchd` service (UAC prompt during install)
- Any GPU works; an NVIDIA card with CUDA 12.x makes indexing fast, Vulkan (AMD/Intel) or CPU otherwise

Download `winseek_<version>_x64-setup.exe` from [Releases](../../releases) (no public release yet — build from source below) and run it. On first launch winseek will:

1. install and start the `searchd` service,
2. offer the model downloads with their sizes up front (~825 MB for search-by-meaning, ~1.5 GB for the document parser — both optional, both resumable),
3. index your chosen folders in the background, pausing on battery or when the GPU is busy.

## Build from source

Prerequisites: Rust (stable, MSVC toolchain) and Node 18+.

```powershell
git clone https://github.com/<you>/winseek.git
cd winseek
powershell -ExecutionPolicy Bypass -File scripts\build-installer.ps1
# -> target\release\bundle\nsis\winseek_0.1.0_x64-setup.exe
```

For development, run the name service in a console (elevated; its stdout is lost in elevated runs, so it logs to a file) and the app against the dev server:

```powershell
cargo run -p searchd -- --console     # elevated terminal
cd app && npm install && npm run tauri dev
```

Tests: `cargo test -p <crate>` per workspace member, `npm test` in `app/ui`.

## Keyboard shortcuts

| Key | Action |
|---|---|
| `Alt+Space` | open / hide winseek |
| type | name results immediately; meaning results follow |
| `↑` `↓` | navigate results |
| `Space` | Quick Look the selected result |
| `Enter` | open |
| `Esc` | hide |

The popup is draggable and remembers where you put it, per monitor.

## FAQ

**How is this different from Everything?** Everything is the gold standard for name search, and winseek's Tier 0 is a study of exactly that approach — but Everything stops at names. winseek adds a second tier that reads what files contain.

**How is this different from DigUp or VectorDash?** Same genre, same models (EmbeddingGemma via llama.cpp) — DigUp is macOS-first, VectorDash is a broader launcher. winseek is Windows-first and puts the service boundary first: a SYSTEM service for names, an unprivileged app for everything else, with the measured VRAM budgeting above. It's also much earlier.

**What gets sent anywhere?** Nothing. The only network traffic is the model downloads you explicitly start.

**Where does everything live?** The app installs to `C:\Program Files\winseek`; the index, models and settings live in `%LOCALAPPDATA%\winseek`. Uninstalling removes the program and service; delete `%LOCALAPPDATA%\winseek` to reclaim the model and index space.

**The installer is flagged by SmartScreen.** It's unsigned (no certificate yet). "More info" → "Run anyway", or build it yourself from source.

## Roadmap

- `size:` and date filters in the name query language
- "Why it matched" explanations, with the matching region highlighted on parsed pages
- Video and audio indexing (transcripts with timestamps, via an FFmpeg sidecar)
- Opt-in text and code indexing, `.gitignore`-aware
- Sort orders and columns beyond name/path in Tier 0

## Acknowledgements

winseek stands on a lot of other people's work:

- **[DigUp](https://github.com/ARahim3/DigUp)** by Abdur Rahim — the direct inspiration for search by meaning on-device; its README is a model of honest technical writing, and its credits chain runs through [VectorDash](https://github.com/ampersandru/VectorDash), winseek's Windows design reference.
- **[Everything](https://www.voidtools.com/)** by David Carpenter — winseek's Tier 0 follows the scan-and-journal architecture studied from the real thing.
- **WizTree** and the **wiztree-rs** project — the raw-$MFT scanning engine winseek's `ntfs` crate is ported from.
- **[llama.cpp](https://github.com/ggml-org/llama.cpp)** — both models run on it; **Google's EmbeddingGemma-2** and **[Infinity-Parser2-Flash](https://huggingface.co/infly/Infinity-Parser2-Flash)** are the models themselves; **pdfium** (via pdfium-render), **sqlite-vec** and **Tauri** hold up the rest.

## License

MIT.
