import { create } from 'zustand';
import { chatLoad, chatSave, hasCJK, translate, type ChatMsg } from '../ipc';

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
  tools: string[];
  /** true = 用户直接在终端敲的输入（不参与发送翻译流） */
  fromTerminal?: boolean;
  /** original snapshot that the current translation was produced from */
  translatedFrom?: string;
  /**
   * JSONL uuid this bubble was reconciled with. Must NOT replace `key`:
   * `key` is the stable local id that in-flight translation callbacks use.
   */
  sessionKey?: string;
}

interface ChatStore {
  byConv: Record<string, Msg[]>;
  /** Loads persisted history once per conversation (no-op if already in memory). */
  hydrate: (convId: string) => Promise<void>;
  addOptimistic: (convId: string, text: string) => string;
  addPlain: (convId: string, text: string) => void;
  upsertFromSession: (convId: string, msgs: ChatMsg[]) => void;
  appendDelta: (convId: string, key: string, delta: string) => void;
  setTranslation: (convId: string, key: string, full: string, fallbackOriginal?: string) => void;
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
          ...saved.filter((m) => !liveKeys.has(m.key)).map((m) => ({
            ...m,
            // A crash mid-translation leaves working=true forever — settle it.
            working: false,
          })),
          ...live,
        ];
        return { byConv: { ...s.byConv, [convId]: merged } };
      });
    } catch {
      /* corrupt cache — start fresh */
    }
  },

  addOptimistic: (convId, text) => {
    const key = `local-${crypto.randomUUID()}`;
    const msg: Msg = {
      key, role: 'user', original: text, translation: null,
      working: true, error: null, ts: null, thinking: null, tools: [],
    };
    set((s) => ({ byConv: { ...s.byConv, [convId]: [...(s.byConv[convId] ?? []), msg] } }));
    return key;
  },

  addPlain: (convId, text) => {
    const msg: Msg = {
      key: `local-${crypto.randomUUID()}`, role: 'user', original: text, translation: null,
      working: false, error: null, ts: null, thinking: null, tools: [], fromTerminal: true,
    };
    set((s) => ({ byConv: { ...s.byConv, [convId]: [...(s.byConv[convId] ?? []), msg] } }));
  },

  upsertFromSession: (convId, msgs) => {
    for (const m of msgs) {
      const list = get().byConv[convId] ?? [];

      if (m.role === 'user') {
        // Reconcile with the optimistic bubble (already-known session key,
        // or matching en translation / original text). Keep `key` stable —
        // an in-flight outbound translation still addresses this bubble by it.
        const trimmed = m.text.trim();
        const match = list.find(
          (x) =>
            x.role === 'user' &&
            (x.sessionKey === m.key ||
              x.translation === trimmed ||
              x.original.trim() === trimmed),
        );
        if (match) {
          set((s) => updateMsg(s.byConv, convId, match.key, (prev) => ({
            ...prev, sessionKey: m.key, ts: prev.ts ?? m.ts,
          })));
          continue;
        }
        if (list.some((x) => x.key === m.key)) continue; // already known
        // Typed directly in the terminal → plain bubble.
        set((s) => ({
          byConv: {
            ...s.byConv,
            [convId]: [...list, {
              key: m.key, role: 'user', original: m.text, translation: null,
              working: false, error: null, ts: m.ts, thinking: null, tools: [],
              fromTerminal: true, sessionKey: m.key,
            } satisfies Msg],
          },
        }));
        continue;
      }

      // assistant: upsert by key
      const existing = list.find((x) => x.key === m.key);
      const textChanged = !existing || existing.original !== m.text;

      if (!existing) {
        const msg: Msg = {
          key: m.key, role: 'assistant', original: m.text, translation: null,
          working: false, error: null, ts: m.ts, thinking: m.thinking, tools: m.tools,
          translatedFrom: undefined,
        };
        set((s) => ({ byConv: { ...s.byConv, [convId]: [...(s.byConv[convId] ?? []), msg] } }));
      } else {
        set((s) => updateMsg(s.byConv, convId, m.key, (prev) => ({
          ...prev,
          original: m.text,
          thinking: m.thinking ?? prev.thinking,
          tools: m.tools.length > 0 ? m.tools : prev.tools,
          ts: m.ts ?? prev.ts,
        })));
      }

      // Schedule translation: immediate when done / already-stale, debounced
      // while streaming so mid-stream snapshots don't burn requests.
      const inflightKey = `${convId}:${m.key}`;
      if (textChanged && !inflight.has(inflightKey)) {
        const current = get().byConv[convId]?.find((x) => x.key === m.key);
        const alreadyCurrent = current?.translatedFrom === m.text && current?.translation;
        if (!alreadyCurrent) {
          if (m.done) {
            cancelSchedule(convId, m.key);
            void get().translateNow(convId, m.key);
          } else {
            schedule(convId, m.key, () => void get().translateNow(convId, m.key));
          }
        }
      }
    }
  },

  appendDelta: (convId, key, delta) =>
    set((s) => updateMsg(s.byConv, convId, key, (m) => ({
      ...m, translation: (m.translation ?? '') + delta,
    }))),

  setTranslation: (convId, key, full, fallbackOriginal) =>
    set((s) => {
      const list = s.byConv[convId] ?? [];
      let target = key;
      if (!list.some((m) => m.key === key) && fallbackOriginal !== undefined) {
        // The bubble was re-keyed/replaced while the request was in flight —
        // land the result on the message that still carries that source text.
        const alt = list.find((m) => m.original === fallbackOriginal);
        if (alt) target = alt.key;
      }
      return updateMsg(s.byConv, convId, target, (m) => ({
        ...m, translation: full, working: false, error: null,
        translatedFrom: m.original,
      }));
    }),

  setError: (convId, key, message) =>
    set((s) => updateMsg(s.byConv, convId, key, (m) => ({ ...m, error: message, working: false }))),

  setWorking: (convId, key, working) =>
    set((s) => updateMsg(s.byConv, convId, key, (m) => ({ ...m, working }))),

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
      set((s) => updateMsg(s.byConv, convId, key, (prev) => ({
        ...prev, translation: prev.original, translatedFrom: prev.original, working: false,
      })));
      return;
    }

    inflight.add(id);
    get().setWorking(convId, key, true);
    const snapshot = m.original;
    try {
      const final = await translate('en-zh', snapshot, (ev) => {
        if (ev.type === 'delta') get().appendDelta(convId, key, ev.text);
        else if (ev.type === 'done') get().setTranslation(convId, key, ev.text, snapshot);
        else if (ev.type === 'error') get().setError(convId, key, ev.message);
      });
      get().setTranslation(convId, key, final, snapshot);
    } catch (e) {
      get().setError(convId, key, String(e));
    } finally {
      inflight.delete(id);
      // Original changed while translating (newer snapshot) → re-run.
      const cur = get().byConv[convId]?.find((x) => x.key === key);
      if (cur && cur.original !== snapshot && !cur.error) {
        void get().translateNow(convId, key);
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
