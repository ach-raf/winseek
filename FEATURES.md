# winseek — everything it can do

winseek is one search box for your whole PC. It finds files by **name** instantly (Everything-speed, straight from the NTFS table), finds them by **meaning and content** (documents, PDFs, screenshots, photos, code), launches **apps and Windows settings**, and gives you a **Library** for your photos, duplicates, disk space and clean-up. It also lets AI agents search your PC through a read-only **MCP server and CLI**.

Contents: [Search](#1-search) · [Results & actions](#2-results--actions) · [Preview](#3-preview) · [Search by meaning](#4-search-by-meaning) · [Code search](#5-code-search) · [Launcher](#6-launcher) · [Library](#7-library) · [Disk space](#8-disk-space) · [Clean-up](#9-clean-up) · [Shortcuts](#10-keyboard-shortcuts) · [Settings](#11-settings) · [AI agents, MCP & CLI](#12-ai-agents-mcp--cli) · [Indexing](#13-indexing-and-live-updates) · [Privacy](#14-privacy-and-safety)

---

## 1. Search

### Instant name search
- Every drive is read directly from the NTFS master file table and then kept current from the USN change journal (new, renamed and deleted files appear within about a second). A full scan of a large system drive takes seconds; later launches restore from a snapshot in under 5 s.
- Works on NTFS, ReFS (via a journal walker), exFAT, FAT and USB drives (via a folder watcher).
- Matching ignores case and accents; every word must match.
- Name hits show **why** they matched, and matched words are highlighted.

### Plain-language search (chips)
Type naturally. Recognised phrases become chips in the bar; everything else is searched as text. Remove a chip with its ✕ or with Backspace at the start of the box, and its words go back into the search.

| You type | winseek understands |
|---|---|
| `photos from last summer` | Pictures + that summer |
| `pdf invoice 2025` | PDF + year 2025 + text "invoice" |
| `big videos` | Video over 100 MB |
| `screenshots this week` | Screenshots from this week |
| `image of a cat` | Pictures + text "cat" |

- **Types:** photos/pictures/pics/images · docs/documents · videos/movies/clips · audio/music/songs/podcasts · folders/directories · apps/programs/installers · settings · files
- **Dates:** today, yesterday, this/last week (Monday start), this/last month, this/last year, `past 7 days` (any count of days/weeks/months/years), seasons (`last summer`, `winter 2025`), months (`march`, `mar 2025`), years (`2024`). Filler words like *from, in, during, since, on* are understood.
- **Sizes:** `big` (>100 MB), `huge` (>1 GB), `small` (<1 MB), or exact: `over 10 mb`, `under 500 kb`, `> 2 gb`, `<= 50mb`. Units b / kb / mb / gb / tb.
- **Extensions:** pdf, docx, xlsx, pptx, csv, txt, md, png, jpg/jpeg, gif, heic, mp4, mkv, mp3, flac, wav, zip, exe, msi (`.pdf` with a dot works too).
- **Screenshots** is its own chip.

### The Filters button
The **Filters** button in the search bar opens "Narrow your search": one-click Type, Date, Size and File-type buttons (click again to remove; picking another in the same row swaps it) and a Power-syntax section that inserts the typed forms below.

### Power syntax
All terms must match (AND).

| Syntax | Meaning |
|---|---|
| `"exact phrase"` | words together, in order |
| `!word` | exclude matches |
| `*` `?` | wildcards (`*.psd`, `report-202?`) |
| `ext:jpg;png` | one or several extensions |
| `size:>50mb`, `size:10mb..1gb` | size comparisons and ranges (`>` `>=` `<` `<=` `A..B`) |
| `folder:` / `file:` | only folders / only files |
| `ws:` | match at the start of a word |
| `docs\2024` | a path fragment limits results to that folder |
| `code:` | search inside source code ([§5](#5-code-search)) |
| `20% of 80`, `(15/4)^2` | built-in calculator ([§6](#6-launcher)) |

### Search box extras
- **Type tabs with live counts** for the typed query: All, Apps, Settings, Images, Docs, Video, Audio, Folders (Tab / Shift+Tab or Alt+1…8).
- **Sort** (Alt+S cycles): best match, name, size, date, type, path, each ascending/descending.
- **Best match ranks files and folders by how useful they are** for what you typed, not A to Z. How the typed word sits in the name decides first: the whole name (`awb`, `awb.xlsx`), then names that start with it, then names with a word that does (`my awb notes.docx`), and the letters inside another word (`drawbar`, a hash) last. Inside each of those: folders before files, then what changed lately, then what sits closer to the top of the drive, and results under build, dependency and cache folders (`target`, `node_modules`, `.git`, `AppData`) cost. With several words typed, the weakest placement decides. Apps and Settings keep the launcher's own ranking (match plus how often and how recently you launched each). A query with nothing typed to rank by (a type tab with an empty box, a pasted path, wildcards) stays in name order.
- **Drive switch**: limit to one drive or search all.
- **Search history**: Alt+↑/↓ steps through earlier queries.
- **Clipboard search**: a separate hotkey opens winseek with your clipboard text as the query.
- **Expand a section** (Alt+→) to see every result of one type, Alt+← to go back.
- **Image grid** results in the popup, navigable with ←/→.
- **"Fix it" suggestions** on empty results: remove a chip, widen the search.

---

## 2. Results & actions

- **Two windows, one app:** a compact popup (global hotkey, default Alt+Space) and a full window with sidebar. Switch with the expand/contract button; set which one opens by default.
- **Result layouts:** list, grid, gallery; rows show type icon or thumbnail, path breadcrumb, size, date.
- **Top result card** with a large icon and a preview of the path, and a **"Most used"** app tile.
- **Open** (Enter), **Show in folder** (Ctrl+Enter), **Run as admin** for apps (Ctrl+Shift+Enter), **Properties** (Alt+Enter), **Open terminal in folder** (Ctrl+Shift+T), **Open folder without selecting** (Ctrl+Shift+E).
- **Rename** in place (F2).
- **Delete → Recycle Bin** (Delete, confirmed by pressing twice) with **Undo** (Ctrl+Z) and an Undo link in the toast.
- **Copy**: the file itself (Ctrl+C, pastes in Explorer), full path (Ctrl+Shift+C), file name (Ctrl+Alt+C).
- **Drag out** a result into other apps, **right-click / Shift+F10 / Ctrl+K** for the full context menu.
- **Multi-select** with Ctrl+click; a running **total size** is shown.
- **Open at a position**: PDFs at a page, media at a timestamp, code at a line, in your default handler when it supports that (VLC, mpv, MPC-HC, VS Code, Notepad++), otherwise the plain open.
- **Toasts** for every action, with Undo where it applies.
- **Update checker** (daily, optional) with a link to the release page.

---

## 3. Preview

Press **Space** on any result. Esc closes.

- **Images**: fit, wheel zoom, drag pan, 1:1 (Ctrl+= / Ctrl+− / Ctrl+0, Alt+Shift+arrows to pan). HEIC supported.
- **PDF**: page-by-page via the bundled PDF reader (Ctrl+PgUp/PgDn between pages).
- **Documents**: Word documents rendered inline; text and code with syntax highlighting; **Markdown** with GitHub-style rendering and a Preview/Source toggle; highlighted code blocks.
- **Video / audio** playback.
- **Document parser** (Ctrl+E): turns a page into text with tables; **copy extracted text** (Ctrl+Shift+X), **copy tables as HTML or Markdown** (pastes into Excel/Word).
- **Match highlighting**: boxes drawn on the image or page where your search words were found (from OCR and parser locations).
- **Path crumbs** and a compact facts strip (size, dimensions, dates).

---

## 4. Search by meaning

Optional. Describe what a file is about, and winseek finds it even when the words differ.

- Covers the folders you choose (default: Pictures, Documents, Desktop, Downloads). Name search always covers every volume.
- **Images**: searches by what is in the picture and by any text in it.
- **Documents and PDFs**: text is extracted and embedded; scanned or image-only pages are read with Windows OCR, and sparse or garbled pages go to an optional **document parser** vision model (tables kept as HTML/Markdown). Unreadable pages are skipped, never guessed.
- **Audio and video**: the soundtrack is embedded in ~30 s overlapping windows, and videos also get one frame per shot (scene change, at least one per minute), embedded like images. No transcript step: audio is matched by meaning directly. Hits open at the matching timestamp. Needs the ffmpeg component (installed on demand); without it audio/video files stay queued.
- **Relevance gate**: results below a score learned from your own index are dropped, so searches return few good answers instead of a long tail.
- **Result refiner (reranker)**: optional second pass that reorders the top hits.
- **Combined ranking**: name, content and meaning hits are fused into one list; each says why it matched.
- **Local models on llama.cpp**: EmbeddingGemma embeddings plus a vision encoder; CUDA → Vulkan → CPU fallback chosen by GPU detection and a health check; models are pinned and SHA-256 checked; servers listen on 127.0.0.1 with a per-launch key and exit after two idle minutes.
- **Performance profiles**: Low, Balanced, Fast background work.
- **Folder scopes** with an estimate (files, time) before you add a folder; per-folder removal; **skipped-names list** (e.g. `Raw*`, `scratch`) to ignore files and folders.
- **Pause / resume**, **rebuild the meaning index**, **index health check**.

---

## 5. Code search

- Turn on **Index source code** in Settings, then search with `code:` ("code: parse config").
- Chunks are line-aware, so each hit carries its **line number** and opens at that line.
- Ranking blends keyword and meaning (hybrid), and favours definitions.
- **Git-aware**: inside a repository, `git ls-files` decides what counts (respects `.gitignore`, global excludes, nested repos); falls back to a built-in `.gitignore` parser without git.

---

## 6. Launcher

The same box launches things.

- **Apps**: Start-menu apps, Store (UWP) apps, **portable programs** (unzipped tools, game launchers, scoop apps found by the name index), and **programs on your PATH**. Uninstallers can be hidden.
- **Aliases**: type `word` for Microsoft Word, `ps` for Photoshop, `taskmgr` for Task Manager; built-in table plus your own aliases.
- **Learning**: your picks are remembered per typed prefix (frecency), so `mp` opens what you always mean. Clear history in Settings.
- **Windows settings and Control Panel** pages, searchable by name.
- **System commands**: lock, sleep, hibernate (only when available), restart, shutdown, sign out, empty Recycle Bin, plus winseek's own panels. Dangerous ones need a second Enter to confirm.
- **Calculator**: `2+2*3`, `(15/4)^2`, `20% of 80`; Enter copies the result.
- **Hide / unhide** items, copy path, reveal, run as admin (context menu with keyboard letters).
- Toggle each source (apps, settings, commands, portable, PATH) in Settings › Launcher.

---

## 7. Library

Open with Ctrl+L, the sidebar, or the tray. Tabs: **Timeline**, **Collections**, **Clean up** (Ctrl+1/2/3).

### Photo timeline
- Every indexed picture, newest first, grouped by year and month with counts and sizes, in a mosaic with feature tiles or a list.
- Views: **All**, **Photos** (camera and saved images), **Screenshots**, **App graphics** (icons, logos and game assets that ship with software are detected without any model and hidden from everything else).
- Name filter, hidden files, month jump, floating preview pane (Space).
- Scales to a million photos by keeping the list in the backend and sending only what is on screen.

### Collections
- A collection is a **saved description** ("a photo of a paper receipt or invoice"); winseek keeps it filled from your photos, screenshots and documents. Starters include Receipts, Whiteboards, Memes, Documents to sign and Screenshots; icons and app graphics stay out.
- A **strictness slider** controls how tight a collection is.
- **Create (Ctrl+N), rename/edit (F2), delete** collections; press X on a tile that does not belong and it stays out.

---

## 8. Disk space

WizTree-style, reachable from the sidebar drive card or Clean up.

- Per-drive bar by file type, folder tree with sizes, and a **treemap**.
- Biggest files list and by-type lists; cached roll-ups so it opens instantly after the scan.
- **Selection total** shown once, consistently, across the header, side panel and Recycle button.
- Select and **recycle** straight from here.

---

## 9. Clean-up

Two tabs: **Suggestions** and **Disk space**. "Nothing is deleted until you say so."

- **Suggestions** summarises what you can free (total, duplicate size, large files you don't open) with three lists:
  - **Duplicates** (keeps the original), with **Review groups** and **Find similar photos** (a background near-duplicate scan).
  - **Large and untouched** files, biggest first, with **See all**.
  - **Old screenshots** older than six months.
- Check rows with Space, Enter to review, Delete to recycle; everything goes to the Recycle Bin (with Undo).

---

## 10. Keyboard shortcuts

Press **F1** or **Ctrl+/** for the live cheat sheet (shows your current bindings).

| Where | Keys |
|---|---|
| Global | **Alt+Space** show/hide (rebindable) |
| Move | ↑↓, PgUp/PgDn, Ctrl+Home/End, Ctrl+↑↓ between sections, ←→ in image grid, Esc back one layer |
| Query | Tab / Shift+Tab type filter, Alt+1…8 jump to a type, Alt+S sort, Backspace removes last chip, Alt+↑↓ history, Alt+→ / Alt+← expand / back |
| Selection | Enter, Space, Ctrl+Enter, Ctrl+Shift+Enter, Alt+Enter, F2, Delete, Ctrl+Z, Shift+F10 / Menu, Ctrl+K, Ctrl+C, Ctrl+Shift+C, Ctrl+Alt+C, Ctrl+Shift+E, Ctrl+Shift+T |
| Windows | Ctrl+L Library, Ctrl+, Settings, Ctrl+Shift+L index health |
| Preview | Esc, Enter, Ctrl+Enter, Ctrl+Shift+C, Ctrl+PgUp/PgDn, Ctrl+= / − / 0, Alt+Shift+arrows, Ctrl+E, Ctrl+Shift+X |
| Library | Ctrl+1/2/3 tabs, Esc/Alt+← back, ↑↓, Enter, Ctrl+N, F2, Delete; Clean-up: Tab, Space, Delete |
| Settings | Tab, Ctrl+1…7 sections, ↑↓, Esc |

Layouts are physical-key based, so they work on AZERTY and other keyboards.

---

## 11. Settings

Search the settings with the **Find a setting** box.

- **General**: open as (popup or full window), start with Windows, daily update check, version, Library options.
- **Appearance**: themes (built-in palettes), **custom theme editor** (a colour for every element, saved), accent colour, window transparency.
- **Shortcuts**: popup, Library and clipboard hotkeys, recorded by pressing the keys; **pause hotkeys** from the tray, and automatically in full-screen games and presentations.
- **Indexing**: folders, source-code indexing, skipped names.
- **Search by meaning**: status, index check, per-component **model downloads** (embedder, llama.cpp runtime, PDF reader, document parser, result refiner) with progress and cancel, performance profile, rebuild.
- **Launcher**: which sources appear, aliases, history.
- **AI agents**: enable the MCP server, choose what agents may read, **activity log**, command-line PATH.
- **Status**: per-drive index state, meaning-index progress, model server rows with unload countdown.

Other UI: a **bottom jobs strip** showing drive scans, meaning indexing, document reading and downloads (click a segment to open its settings), a first-run setup with a folder estimate, a system-tray icon and autostart.

---

## 12. AI agents, MCP & CLI

### MCP server (`winseek-mcp.exe`)
Read-only, stdio JSON-RPC, off by default (Settings › AI agents). Every call is logged on the settings page. Connect Claude Code, Claude Desktop, Cursor, VS Code or Codex with the snippets provided.

| Tool | What it does |
|---|---|
| `search_files` | name search with the full syntax, sort and date/size filters |
| `search_by_meaning` | semantic search over indexed content |
| `read_text` | read a file's text (with page) |
| `parse_document` | run the document parser on a page |
| `recent_files` | most recently changed files |
| `file_info` | metadata for a path |
| `find_apps` | installed apps, settings pages, commands |
| `library_big_files`, `library_duplicates`, `library_collections` | the Library's big files, duplicate groups and collections |
| `index_status` | index health (always available) |

Agents cannot open, launch, rename or delete anything.

### Command line (`winseek-cli.exe`)

```
winseek-cli search <query...> [-n N] [--sort ORDER] [--long] [--json]
winseek-cli status [--json]
winseek-cli index [VOLUME | --all] [--wait] [--timeout SECS]
winseek-cli meaning <query...> [-n N] [--long] [--json]
winseek-cli read <path> [--page N] [--max N] [--json]
winseek-cli recent [-n N] [--json]
winseek-cli apps <query...> [-n N] [--kind app|setting|command] [--json]
winseek-cli path [status | add | remove] [--machine] [--json]
```

Sort orders: name, size, date, type, path, each with `_asc` / `_desc`. `search`, `status` and `index` work with the app closed; the rest need the app with agent access on. Paths print one per line, `--json` on any command.

---

## 13. Indexing and live updates

- **Tier 0 (name index)** runs as a Windows service (`searchd`) and never reads file contents.
- **Tier 1 (content)** runs in your user account only: crawler, text extraction (documents, PDFs, code, media metadata), OCR, embeddings, document parser, picture classification, near-duplicate scan.
- Index lives in one SQLite file (FTS5 full-text plus vector table) with automatic recovery from damage; snapshots keep launches instant.
- Rescan a drive on demand; a stuck scan is detected.
- Background work respects your profile and steps aside for games.

---

## 14. Privacy and safety

- No account, no telemetry. The only network traffic is model downloads you start yourself (and the optional update check).
- File contents and models are never touched by the SYSTEM service.
- Local model servers bind to 127.0.0.1 with a per-launch key.
- MCP/CLI access is read-only and off until you enable it; the pipe is restricted to your user.
- Deleting uses the Recycle Bin with Undo.

## Not built yet
Sorting by created/accessed time (date sort is modified time only), and word-for-word transcripts of speech (audio is searched by meaning, not transcribed).
