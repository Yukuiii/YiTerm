import { create } from 'zustand';
import { chatLoad, chatSave, debugLog, hasCJK, translate, type ChatMsg, type ToolCall } from '../ipc';
import { useSettingsStore } from './settings';

export interface ToolCallItem extends ToolCall {
  /** Output text once the tool_result line arrives. */
  result?: string;
  isError?: boolean;
}

function mergeToolCalls(prev: ToolCallItem[], incoming: ToolCall[]): ToolCallItem[] {
  const byId = new Map(prev.map((t) => [t.id, t]));
  const merged: ToolCallItem[] = incoming.map((t) => {
    const old = byId.get(t.id);
    return old ? { ...t, result: old.result, isError: old.isError } : { ...t };
  });
  for (const t of prev) {
    if (!merged.some((x) => x.id === t.id)) merged.push(t);
  }
  return merged;
}

export interface Msg {
  key: string;
  role: 'user' | 'assistant';
  /** user: 中文原文（终端直敲时为敲入文本）; assistant: 英文原文 */
  original: string;
  /** user: 英文译文; assistant: 中文译文; null = 尚未翻译 */
  translation: string | null;
  working: boolean;
  error: string | null;
  ts: string | null;
  thinking: string | null;
  toolCalls: ToolCallItem[];
  /** true = 用户直接在终端敲的输入（不参与发送翻译流） */
  fromTerminal?: boolean;
  /**
   * Prefix of `original` that the current `translation` covers. For cloud
   * modes this is the whole snapshot; for local (incremental) translation it
   * is the last sentence/paragraph boundary that was translated.
   */
  translatedFrom?: string;
  /** Assistant turn finished streaming; the trailing fragment may be translated. */
  done?: boolean;
  /**
   * English text actually written into the terminal for this user bubble.
   * The JSONL stores that text, so this is the deterministic reconcile key
   * (translation may still be in flight when the line arrives).
   */
  sentText?: string;
  /**
   * JSONL uuid this bubble was reconciled with. Must NOT replace `key`:
   * `key` is the stable local id that in-flight translation callbacks use.
   */
  sessionKey?: string;
  /**
   * Absolute paths of screenshots pasted into the composer, persisted with the
   * message so history can render thumbnails. The files live in the app cache
   * and the OS may reclaim them, so rendering tolerates a missing file.
   */
  images?: string[];
  /**
   * Persisted counterpart of `images`: records that the turn had screenshots
   * without keeping paths that will be dead after a restart, so history can
   * still say "（图片）" instead of looking like an empty prompt.
   */
  hadImages?: boolean;
}

interface ChatStore {
  byConv: Record<string, Msg[]>;
  /**
   * Prompt queued by the landing screen before the conversation's terminal is
   * ready; consumed by ChatPanel once it can write to the PTY.
   */
  pendingPrompts: Record<string, string>;
  queuePrompt: (convId: string, text: string) => void;
  takePrompt: (convId: string) => string | null;
  /** Loads persisted history once per conversation (no-op if already in memory). */
  hydrate: (convId: string) => Promise<void>;
  addOptimistic: (convId: string, text: string, images?: string[]) => string;
  addPlain: (convId: string, text: string, images?: string[]) => void;
  markSent: (convId: string, key: string, sentText: string) => void;
  upsertFromSession: (convId: string, msgs: ChatMsg[], translate?: boolean) => void;
  appendDelta: (convId: string, key: string, delta: string) => void;
  setTranslation: (
    convId: string,
    key: string,
    full: string,
    fallbackOriginal?: string,
    translatedFrom?: string,
  ) => void;
  setError: (convId: string, key: string, message: string) => void;
  setWorking: (convId: string, key: string, working: boolean) => void;
  clearConv: (convId: string) => void;
  /** 立即翻译一条消息（assistant en→zh） */
  translateNow: (convId: string, key: string) => Promise<void>;
  /** 发送方向的翻译（zh→en），返回最终英文 */
  translateOutbound: (convId: string, key: string, text: string) => Promise<string>;
}

// ---- module-level translation scheduling state ----
const debounceTimers = new Map<string, number>(); // `${convId}:${key}` → timeout id
const inflight = new Set<string>();
const hydrated = new Set<string>();
const lastSaved = new Map<string, string>();
let saveTimer: number | undefined;
const DEBOUNCE_MS = 1200;

function schedule(convId: string, key: string, cb: () => void) {
  const id = `${convId}:${key}`;
  const existing = debounceTimers.get(id);
  if (existing !== undefined) window.clearTimeout(existing);
  const timer = window.setTimeout(() => {
    debounceTimers.delete(id);
    cb();
  }, DEBOUNCE_MS);
  debounceTimers.set(id, timer);
}

function cancelSchedule(convId: string, key: string) {
  const id = `${convId}:${key}`;
  const existing = debounceTimers.get(id);
  if (existing !== undefined) {
    window.clearTimeout(existing);
    debounceTimers.delete(id);
  }
}

/** True if `pos` falls inside a fenced code block of `text`. */
function insideFence(text: string, pos: number): boolean {
  let fences = 0;
  let lineStart = 0;
  for (const line of text.split('\n')) {
    if (lineStart >= pos) break;
    if (/^\s{0,3}(?:```|~~~)/.test(line)) fences++;
    lineStart += line.length + 1;
  }
  return fences % 2 === 1;
}

/**
 * Index just past the last completed paragraph or sentence of `text`,
 * skipping boundaries that fall inside a fenced code block. Returns 0 when
 * nothing complete is available yet.
 */
function completeBoundary(text: string): number {
  const candidates: number[] = [];
  for (let i = text.indexOf('\n\n'); i !== -1; i = text.indexOf('\n\n', i + 2)) {
    candidates.push(i + 2);
  }
  const re = /[.!?。！？](?:["')\]]*)(?=\s|$)/g;
  for (let match; (match = re.exec(text));) {
    candidates.push(match.index + match[0].length);
  }
  candidates.sort((a, b) => b - a);
  for (const cut of candidates) {
    if (!insideFence(text, cut)) return cut;
  }
  return 0;
}

function updateMsg(byConv: Record<string, Msg[]>, convId: string, key: string, f: (m: Msg) => Msg): Record<string, Msg[]> {
  const list = byConv[convId];
  if (!list) return byConv;
  const idx = list.findIndex((m) => m.key === key);
  if (idx === -1) return byConv;
  const next = [...list];
  next[idx] = f(next[idx]);
  return { ...byConv, [convId]: next };
}

export const useChatStore = create<ChatStore>((set, get) => ({
  byConv: {},
  pendingPrompts: {},

  queuePrompt: (convId, text) =>
    set((s) => ({ pendingPrompts: { ...s.pendingPrompts, [convId]: text } })),

  takePrompt: (convId) => {
    const text = get().pendingPrompts[convId] ?? null;
    if (text !== null) {
      set((s) => {
        const pendingPrompts = { ...s.pendingPrompts };
        delete pendingPrompts[convId];
        return { pendingPrompts };
      });
    }
    return text;
  },

  hydrate: async (convId) => {
    if (hydrated.has(convId)) return;
    hydrated.add(convId);
    try {
      const raw = await chatLoad(convId);
      if (!raw) return;
      const saved = JSON.parse(raw) as Msg[];
      if (!Array.isArray(saved)) return;
      // Don't clobber anything that arrived while loading (session-updates can
      // race a fresh start): merge by key, saved first.
      set((s) => {
        const live = s.byConv[convId] ?? [];
        const liveKeys = new Set(live.map((m) => m.key));
        const merged = [
          ...saved.filter((m) => !liveKeys.has(m.key)).map((m) => {
            const restored: Msg = {
              ...m,
              // A crash mid-translation leaves working=true forever — settle it.
              working: false,
              // Older caches predate tool capture.
              toolCalls: m.toolCalls ?? [],
            };
            return restored;
          }),
          ...live,
        ];
        return { byConv: { ...s.byConv, [convId]: merged } };
      });
    } catch {
      /* corrupt cache — start fresh */
    }
  },

  addOptimistic: (convId, text, images) => {
    const key = `local-${crypto.randomUUID()}`;
    const msg: Msg = {
      key, role: 'user', original: text, translation: null,
      working: true, error: null,
      // An image-bearing turn never reconciles with the transcript (its JSONL
      // content is an array), so it needs its own timestamp now.
      ts: images?.length ? new Date().toISOString() : null,
      thinking: null, toolCalls: [],
      images: images?.length ? images : undefined,
      hadImages: images?.length ? true : undefined,
    };
    set((s) => ({ byConv: { ...s.byConv, [convId]: [...(s.byConv[convId] ?? []), msg] } }));
    if (images?.length) saveNow(convId);
    void debugLog(`addOptimistic conv=${convId} key=${key} len=${text.length} img=${images?.length ?? 0}`);
    return key;
  },

  markSent: (convId, key, sentText) =>
    set((s) => ({ byConv: updateMsg(s.byConv, convId, key, (m) => ({ ...m, sentText })) })),

  addPlain: (convId, text, images) => {
    const msg: Msg = {
      key: `local-${crypto.randomUUID()}`, role: 'user', original: text, translation: null,
      working: false, error: null,
      ts: images?.length ? new Date().toISOString() : null,
      thinking: null, toolCalls: [],
      images: images?.length ? images : undefined,
      hadImages: images?.length ? true : undefined,
    };
    set((s) => ({ byConv: { ...s.byConv, [convId]: [...(s.byConv[convId] ?? []), msg] } }));
    if (images?.length) saveNow(convId);
  },

  upsertFromSession: (convId, msgs, translate = true) => {
    void debugLog(
      `upsert conv=${convId} n=${msgs.length} ` +
        msgs.map((m) => `${m.role}:${m.key.slice(0, 12)}:len${m.text.length}:done${m.done ? 1 : 0}`).join(','),
    );
    for (const m of msgs) {
      const list = get().byConv[convId] ?? [];

      // Tool output: attach to the assistant message holding that call id.
      if (m.toolResult) {
        const tr = m.toolResult;
        set((s) => {
          const cur = s.byConv[convId] ?? [];
          let hit = false;
          const next = cur.map((msg) => {
            if (msg.toolCalls.length === 0) return msg;
            const idx = msg.toolCalls.findIndex((t) => t.id === tr.callId);
            if (idx === -1) return msg;
            hit = true;
            const calls = [...msg.toolCalls];
            calls[idx] = { ...calls[idx], result: tr.text, isError: tr.isError };
            return { ...msg, toolCalls: calls };
          });
          return hit ? { byConv: { ...s.byConv, [convId]: next } } : {};
        });
        continue;
      }

      if (m.role === 'user') {
        // Reconcile with the optimistic bubble (already-known session key,
        // or matching en translation / original text). Keep `key` stable —
        // an in-flight outbound translation still addresses this bubble by it.
        const trimmed = m.text.trim();
        // When Claude Code cannot attach a pasted image it falls back to
        // inserting the literal path as text, which arrives here as a
        // string-content turn. Strip our own image paths (and any `[Image #N]`
        // marker) so it reconciles instead of becoming a second bubble whose
        // body is a filesystem path.
        const match = list.find((x) => {
          if (x.role !== 'user') return false;
          if (x.sessionKey === m.key) return true;
          if (x.sentText === trimmed || x.translation === trimmed || x.original.trim() === trimmed) {
            return true;
          }
          if (!x.images?.length) return false;
          let rest = trimmed;
          for (const p of x.images) rest = rest.split(p).join(' ');
          rest = rest.replace(/\[(?:Image|Pasted text) #\d+(?: \+\d+ lines)?\]/g, '').trim();
          return rest !== trimmed && (x.sentText === rest || x.original.trim() === rest);
        });
        if (match) {
          set((s) => ({ byConv: updateMsg(s.byConv, convId, match.key, (prev) => ({
            ...prev, sessionKey: m.key, ts: prev.ts ?? m.ts,
          })) }));
          continue;
        }
        if (list.some((x) => x.key === m.key)) continue; // already known
        // Rebuilding a turn we have no optimistic bubble for (e.g. an image
        // turn restored from the transcript, or one typed in the terminal).
        // Strip Claude Code's `[Image #N]` marker: the thumbnail itself is not
        // persisted, so record that the turn had one and keep the caption.
        const hadImages = /\[Image #\d+\]/.test(m.text);
        const display = m.text.replace(/\[Image #\d+\]/g, '').replace(/\s+/g, ' ').trim();
        set((s) => ({
          byConv: {
            ...s.byConv,
            [convId]: [...(s.byConv[convId] ?? []), {
              key: m.key, role: 'user', original: display, translation: null,
              working: false, error: null, ts: m.ts, thinking: null, toolCalls: [],
              fromTerminal: true, sessionKey: m.key,
              hadImages: hadImages || undefined,
            } satisfies Msg],
          },
        }));
        continue;
      }

      // assistant: upsert by key.
      // A message id can arrive as several block-scoped lines (thinking / text /
      // tool_use) — never let an empty-text line wipe text we already have.
      const existing = list.find((x) => x.key === m.key);
      const nextText = m.text !== '' ? m.text : (existing?.original ?? '');
      const textChanged = existing ? nextText !== existing.original : m.text !== '';
      void debugLog(
        `asstIter key=${m.key.slice(0, 14)} existed=${existing ? 1 : 0} ` +
          `prevLen=${existing ? existing.original.length : -1} newLen=${m.text.length} ` +
          `changed=${textChanged ? 1 : 0} think=${m.thinking ? 1 : 0} tools=${m.toolCalls.length}`,
      );

      if (!existing) {
        const msg: Msg = {
          key: m.key, role: 'assistant', original: m.text, translation: null,
          working: false, error: null, ts: m.ts, thinking: m.thinking,
          toolCalls: mergeToolCalls([], m.toolCalls),
          translatedFrom: undefined, done: m.done,
        };
        set((s) => ({ byConv: { ...s.byConv, [convId]: [...(s.byConv[convId] ?? []), msg] } }));
      } else {
        set((s) => ({ byConv: updateMsg(s.byConv, convId, m.key, (prev) => ({
          ...prev,
          original: nextText,
          thinking: m.thinking ?? prev.thinking,
          toolCalls: mergeToolCalls(prev.toolCalls, m.toolCalls),
          ts: m.ts ?? prev.ts,
          // Block-scoped lines arrive separately; once a turn is done it
          // must stay done even if a late line reports false.
          done: prev.done || m.done,
        })) }));
      }

      // Schedule translation: immediate when done / already-stale, debounced
      // while streaming so mid-stream snapshots don't burn requests. Replayed
      // history from Claude's records is displayed untranslated.
      const inflightKey = `${convId}:${m.key}`;
      const current = get().byConv[convId]?.find((x) => x.key === m.key);
      const fullyTranslated =
        !!current?.translation && current.translatedFrom === current.original;
      // Translate on new text, and once more when the turn ends so the
      // trailing (possibly incomplete) fragment gets covered too.
      if (translate && (textChanged || (m.done && !fullyTranslated)) && !inflight.has(inflightKey)) {
        if (m.done) {
          cancelSchedule(convId, m.key);
          void get().translateNow(convId, m.key);
        } else {
          schedule(convId, m.key, () => void get().translateNow(convId, m.key));
        }
      }
    }
    // Ground truth of the live store after the batch (persistence is
    // timer-based and can lag under webview throttling).
    void debugLog(
      `upsertDone conv=${convId} tail=` +
        (get().byConv[convId] ?? [])
          .slice(-3)
          .map((x) => `${x.key.slice(0, 10)}:len${x.original.length}:tr${(x.translation ?? '').length}:w${x.working ? 1 : 0}`)
          .join(','),
    );
  },

  appendDelta: (convId, key, delta) =>
    set((s) => ({ byConv: updateMsg(s.byConv, convId, key, (m) => ({
      ...m, translation: (m.translation ?? '') + delta,
    })) })),

  setTranslation: (convId, key, full, fallbackOriginal, translatedFrom) =>
    set((s) => {
      const list = s.byConv[convId] ?? [];
      let target = key;
      if (!list.some((m) => m.key === key) && fallbackOriginal !== undefined) {
        // The bubble was re-keyed/replaced while the request was in flight —
        // land the result on the message that still carries that source text.
        const alt = list.find((m) => m.original === fallbackOriginal);
        if (alt) target = alt.key;
      }
      void debugLog(
        `setTranslation conv=${convId} key=${key.slice(0, 16)} target=${target.slice(0, 16)} ` +
          `found=${list.some((m) => m.key === target) ? 1 : 0} outLen=${full.length}`,
      );
      return {
        byConv: updateMsg(s.byConv, convId, target, (m) => ({
          ...m, translation: full, working: false, error: null,
          translatedFrom: translatedFrom ?? m.original,
        })),
      };
    }),

  setError: (convId, key, message) =>
    set((s) => ({ byConv: updateMsg(s.byConv, convId, key, (m) => ({ ...m, error: message, working: false })) })),

  setWorking: (convId, key, working) =>
    set((s) => ({ byConv: updateMsg(s.byConv, convId, key, (m) => ({ ...m, working })) })),

  clearConv: (convId) =>
    set((s) => {
      const byConv = { ...s.byConv };
      delete byConv[convId];
      return { byConv };
    }),

  translateNow: async (convId, key) => {
    const id = `${convId}:${key}`;
    if (inflight.has(id)) return;
    const m = get().byConv[convId]?.find((x) => x.key === key);
    if (!m || !m.original.trim()) return;
    // Already Chinese (or contains CJK) → show as-is, skip translation.
    if (m.role === 'assistant' && hasCJK(m.original) && !/[a-zA-Z]{8,}/.test(m.original)) {
      set((s) => ({ byConv: updateMsg(s.byConv, convId, key, (prev) => ({
        ...prev, translation: prev.original, translatedFrom: prev.original, working: false,
      })) }));
      return;
    }

    // Local inference decodes at ~12 tok/s, so re-translating a growing
    // snapshot from scratch would stall for seconds after every update.
    // Instead translate only the text not covered yet, in sentence- or
    // paragraph-sized pieces, and append it to the existing translation.
    const local = useSettingsStore.getState().settings?.translation.mode === 'local';
    const covered = m.translatedFrom ?? '';
    // `!m.error`: a failed run may have appended garbage deltas to
    // `translation`; restart from scratch rather than extending it.
    const extend = local && !m.error && !!m.translation && covered.length > 0 &&
      m.original.startsWith(covered);
    let from = extend ? covered.length : 0;
    let to = m.original.length;
    if (local) {
      if (!m.done) {
        const boundary = completeBoundary(m.original);
        if (boundary <= from) return; // nothing complete to translate yet
        to = boundary;
      }
      // Otherwise (done) translate everything, including the last fragment.
    }

    const tail = m.original.slice(from, to);
    if (!tail.trim()) {
      if (m.done) {
        get().setTranslation(convId, key, m.translation ?? '', m.original, m.original);
      }
      return;
    }

    inflight.add(id);
    get().setWorking(convId, key, true);
    const snapshot = m.original;
    const coverTo = snapshot.slice(0, to);
    const base = from > 0 ? (m.translation ?? '') : '';
    if (from === 0 && (m.translation || m.error)) {
      // Full re-translation: clear stale text/error so deltas don't pile
      // onto a previous run.
      set((s) => ({ byConv: updateMsg(s.byConv, convId, key, (prev) => ({
        ...prev, translation: '', error: null,
      })) }));
    }
    try {
      const final = await translate('en-zh', tail, (ev) => {
        if (ev.type === 'delta') get().appendDelta(convId, key, ev.text);
        else if (ev.type === 'done') get().setTranslation(convId, key, base + ev.text, snapshot, coverTo);
        else if (ev.type === 'error') get().setError(convId, key, ev.message);
      });
      get().setTranslation(convId, key, base + final, snapshot, coverTo);
    } catch (e) {
      get().setError(convId, key, String(e));
    } finally {
      inflight.delete(id);
      // New text arrived (or the turn finished) while we were translating.
      // Continue from where the translation now ends — only the newly
      // covered tail is translated, never the whole message again.
      const cur = get().byConv[convId]?.find((x) => x.key === key);
      if (cur && !cur.error) {
        const coveredNow = cur.translatedFrom ?? '';
        const uncovered =
          !cur.original.startsWith(coveredNow) || cur.original.length > coveredNow.length;
        const finished = !!cur.done && !cur.working;
        if (uncovered && (finished || cur.original !== snapshot)) {
          void get().translateNow(convId, key);
        }
      }
    }
  },

  translateOutbound: async (convId, key, text) => {
    const final = await translate('zh-en', text, (ev) => {
      if (ev.type === 'delta') get().appendDelta(convId, key, ev.text);
      else if (ev.type === 'done') get().setTranslation(convId, key, ev.text, text);
      else if (ev.type === 'error') get().setError(convId, key, ev.message);
    });
    get().setTranslation(convId, key, final, text);
    return final;
  },
}));

/**
 * Writes one conversation immediately instead of waiting for the debounce.
 * Image-bearing turns need this: they are the only record of the attachment
 * paths, so quitting inside the 500 ms window would lose the thumbnail.
 */
function saveNow(convId: string) {
  const msgs = useChatStore.getState().byConv[convId];
  if (!msgs) return;
  const json = JSON.stringify(msgs);
  lastSaved.set(convId, json);
  void chatSave(convId, json).catch(() => lastSaved.delete(convId));
}

// Debounced persistence: streaming deltas fire very often, so only write the
// conversations whose serialized form actually changed, at most ~2x/second.
useChatStore.subscribe((state, prev) => {
  if (state.byConv === prev.byConv) return;
  if (saveTimer !== undefined) window.clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => {
    for (const [convId, msgs] of Object.entries(useChatStore.getState().byConv)) {
      const json = JSON.stringify(msgs);
      if (lastSaved.get(convId) === json) continue;
      lastSaved.set(convId, json);
      void chatSave(convId, json).catch(() => {
        lastSaved.delete(convId); // retry on next change
      });
    }
  }, 500);
});
