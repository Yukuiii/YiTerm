# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

YiTerm is a macOS desktop shell (Tauri 2 + React 19) around the real `claude` CLI. It is **not** an API client: it spawns `claude` in a PTY and mirrors the conversation from Claude Code's own JSONL transcript. Chinese prompts are translated zh → en on the way into the PTY; English replies are read from the transcript and translated en → zh for display. All user-facing UI strings are Simplified Chinese (`index.html` is `lang="zh-CN"`).

## Commands

| Command | Notes |
| --- | --- |
| `npm run tauri dev` | Full app with HMR (Vite on :5173 + Rust backend) |
| `npm run dev` | Frontend only |
| `npm run build` | `tsc -b && vite build` |
| `npm run lint` | oxlint (config: `.oxlintrc.json`) |
| `npm test` | All frontend tests (`node --experimental-vm-modules --test tests/*.test.mjs`) |
| `node --experimental-vm-modules --test tests/chat.test.mjs` | One test file |
| `node --experimental-vm-modules --test --test-name-pattern="lane" tests/gitGraph.test.mjs` | One test case |
| `cd src-tauri && cargo test` | Rust unit tests (163, mostly in-module `#[cfg(test)]`) |
| `cd src-tauri && cargo test munge_rules` | Single Rust test (substring filter) |

There is no CI config, no `.github`, no Cursor/Copilot rules.

Release = version bump in `package.json`, `package-lock.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`, `src-tauri/tauri.conf.json`, committed as `chore(release): x.y.z`. Feature commits use Conventional Commits with a scope: `feat(chat): …`, `fix(git): …`, `refactor(sidebar): …`.

## Architecture

```
React (src/)  ──Tauri IPC / Channels──  Rust (src-tauri/src/)
  chat mirror, translation client,        PTY (portable-pty), JSONL follower,
  xterm.js, sidebar, Git views            translate (Google/DeepL/Chat/Local),
                                          settings + keys, conversations, git
```

Two independent pipelines meet in the frontend chat store:

1. **Inbound**: `src/components/ChatPanel.tsx` → `translate('zh-en')` (skipped when the text has no CJK) → `terminal_send_prompt` → PTY.
2. **Outbound**: `sessions/follower.rs` polls the transcript every 300 ms → `session-update` event → `src/stores/chat.ts` `upsertFromSession` → `translate('en-zh')` → message bubble.

Nothing reads Claude's replies from the PTY stream (that is xterm-only display); the transcript is the single source of truth for chat content. Anything that must survive a restart is written to `chats/<conv-id>.json` by the frontend.

### Rust backend

- `ipc.rs` — all Tauri commands and the cross-cutting lifecycle rules: at most `MAX_LIVE_TERMINALS` (4) live `claude` processes with LRU reclaim of idle terminals, conversation adoption for sessions created outside YiTerm, chat/image/attachment persistence.
- `terminal/mod.rs` — PTY spawn, reader + coalescing flusher threads, 1 MiB backlog ring replayed on re-attach, and the composer's write sequencing.
- `terminal/plan_approval.rs` — proves an `ExitPlanMode` approval menu is actually on screen before letting the chat press a digit. Fail-closed by design.
- `terminal/buffer.rs` — the byte ring with sequence numbers behind `attach`/replay.
- `sessions/mod.rs` — `parse_line` turns one transcript line into `ChatMsg`s; also owns `conversations.json`.
- `sessions/follower.rs` — transcript tail: turn-busy state, interrupt markers, in-place rewrite/replacement detection, historical-vs-live batching.
- `sessions/claude.rs` — discovery of `~/.claude.json` projects and existing transcripts for the sidebar.
- `translate/mod.rs` — chunking, code/paragraph protection, provider dispatch; `google.rs`, `deepl.rs`, `oneshot.rs` (built-in anonymous DeepL channel, mode `dlx`), `chat.rs` (OpenAI-compatible SSE), `local.rs` (Hy-MT2-1.8B via the vendored `vendor/hy-mt-core` crate, Candle CPU).
- `git.rs` — shells out to `git` with output caps and a 10 s deadline for status/diff/history/commit details.
- `update.rs` — GitHub Releases check + in-place DMG swap (the app is unsigned, so the official Tauri updater is unusable).
- `state.rs` — `AppState`, and `scutil`-based system proxy detection (a Dock-launched app has no shell env).

### Frontend

- `src/stores/chat.ts` is the core: reconciliation, translation scheduling, debounced persistence. It also holds module-level state (debounce timers, in-flight set, hydration set) that is deliberately outside the store.
- Other stores: `conversations.ts` (re-scans Claude's records every 5 s while visible), `terminals.ts` (dedupes concurrent `terminal_start`), `settings.ts`.
- Pure logic lives in standalone modules covered by `node:test`: `gitGraph.ts`, `gitHistory.ts`, `gitCommitDetails.ts`, `links.ts`, `planApproval.ts`, `toolActivity.ts`, `updates.ts`, plus `hooks/useComposerKeyboard.ts` and `stores/chat.ts`. Prefer extending these over adding logic to components.
- `src/ipc.ts` is the single typed wrapper over `invoke`/`listen`/`Channel`; types there must stay in sync with the Rust `#[serde(rename_all = "camelCase")]` structs.

### Frontend test harness

`tests/*.test.mjs` load the real `src/` source into a `node:vm` context — `.ts` through `module.stripTypeScriptTypes`, `.tsx` through Vite's `transformWithOxc` and rendered with `renderToStaticMarkup` — with every import supplied explicitly by the test file. No webview, no extra test dependencies. Consequences:

- Tested `.ts` modules must stay within **erasable** TypeScript syntax (`tsconfig.app.json` sets `erasableSyntaxOnly`) — no `enum`, no parameter properties, no namespaces.
- Each test lists the source files and external modules it links. A new import in a tested file (or a new file in its import graph) must be added to that test's `files` / mock exports, otherwise linking throws `Unexpected import`.

## Invariants worth reading before changing things

- **Message identity**: user bubbles are keyed by the transcript line `uuid`, assistant messages by `message.id` (Claude Code writes several streaming snapshots per id, so these upsert). The frontend's optimistic bubble keeps its own `local-*` key forever; reconciliation uses `sentText` (the exact English written into the PTY) plus a tolerant text match, because translation may still be in flight when the transcript line lands. Never re-key a bubble that an in-flight translation addresses.
- **PTY write timing**: text and Enter must be separate writes (`ENTER_SETTLE`, 250 ms) — a burst with `\r` is treated as a paste and the Enter is swallowed. Image paths go as a bracketed paste and the code waits for the `[Image #N]` marker to be rendered before pressing Enter.
- **Translation protection**: fenced/inline code and paragraph breaks are replaced with `@@C<n>@@` sentinels before translation and restored after; `safe_stream_prefix` prevents flushing a partial sentinel mid-stream. Local mode additionally translates only up to the last complete sentence boundary and appends.
- **Plan approval is fail-closed**: a decision requires a live transcript `ExitPlanMode` candidate *and* a recognized on-screen menu *and* a matching token. Any raw input, resize, redraw, retirement, or owner/session mismatch invalidates the token. Do not add a path that writes a digit without going through `PlanApprovalTracker::consume`.
- **Lock order** is `plan_approval` tracker → `writer`/`master`; no `std` guard may cross an `await` (Tauri async commands must stay `Send`).
- **Imported sessions** run with the cwd string Claude recorded, verbatim (canonicalizing it can make `--resume` look in a different project directory); their replayed history is displayed untranslated.
- **Esc** is the interrupt key only as a lone `\x1b`. The backend clears the busy flag itself because an abort before the first assistant line writes no transcript marker.
- **Child env**: per-session `CLAUDE_CODE_*` markers are stripped and `CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1` is set, so the child always writes the transcript the follower depends on.
- The app is tightly coupled to Claude Code internals verified against **2.1.316**: the JSONL line shapes, the plan-selector's exact English labels (`terminal/plan_approval.rs`), the transcript directory munging (`sessions::munge_dir`), and the `[Image #N]` marker. A CLI upgrade can break these silently.

## Debugging

- Logs: `~/Library/Logs/com.yiterm.app/yiterm.log` (frontend breadcrumbs come through `debug_log` with target `yiterm_frontend`).
- App data: `~/Library/Application Support/com.yiterm.app/` — `settings.json`, `keys.json` (0600), `conversations.json`, `chats/<id>.json`.
- Pasted screenshots: `~/Library/Caches/com.yiterm.app/images/<conv-id>/` (disposable; the UI tolerates missing files).
- New Tauri commands must be registered in `lib.rs`'s `invoke_handler`; new opener/fs permissions go in `src-tauri/capabilities/default.json`.
