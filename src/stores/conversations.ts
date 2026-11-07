import { create } from 'zustand';
import {
  conversationsCreate,
  conversationsDelete,
  conversationsList,
  type Conversation,
} from '../ipc';

interface ConversationsStore {
  list: Conversation[];
  /** Project directories registered in ~/.claude.json (may have no sessions). */
  projects: string[];
  activeId: string | null;
  loading: boolean;
  load: () => Promise<void>;
  /** Re-reads Claude's records; used to pick up sessions created outside. */
  refresh: () => Promise<void>;
  create: (folder: string) => Promise<Conversation>;
  remove: (id: string) => Promise<void>;
  setActive: (id: string | null) => void;
}

// Invalidates in-flight refreshes when the list is mutated or a newer refresh
// starts, so a slow scan can never resurrect a deleted row or drop a new one.
let refreshTick = 0;

export const useConversationsStore = create<ConversationsStore>((set, get) => ({
  list: [],
  projects: [],
  activeId: null,
  loading: false,

  load: async () => {
    set({ loading: true });
    const { conversations, projects } = await conversationsList();
    refreshTick++;
    set({ list: conversations, projects, loading: false });
    if (!get().activeId) {
      // Never auto-open a session imported from Claude's records: that would
      // resume a PTY and replay/translate its whole history on launch.
      const firstOwn = conversations.find((c) => !c.imported);
      if (firstOwn) set({ activeId: firstOwn.id });
    }
  },

  refresh: async () => {
    const tick = ++refreshTick;
    try {
      const { conversations, projects } = await conversationsList();
      if (tick !== refreshTick) return;
      set((s) => {
        const unchanged =
          s.projects.length === projects.length &&
          s.projects.every((p, i) => p === projects[i]) &&
          s.list.length === conversations.length &&
          conversations.every((c, i) => {
            const prev = s.list[i];
            return (
              prev.id === c.id &&
              prev.folder === c.folder &&
              prev.title === c.title &&
              prev.createdAt === c.createdAt &&
              !!prev.imported === !!c.imported
            );
          });
        return unchanged ? {} : { list: conversations, projects };
      });
    } catch {
      /* keep the last known list; the next tick retries */
    }
  },

  create: async (folder) => {
    const conv = await conversationsCreate(folder);
    refreshTick++;
    set((s) => ({ list: [conv, ...s.list], activeId: conv.id }));
    return conv;
  },

  remove: async (id) => {
    await conversationsDelete(id);
    refreshTick++;
    set((s) => {
      const list = s.list.filter((c) => c.id !== id);
      const activeId = s.activeId === id ? (list[0]?.id ?? null) : s.activeId;
      return { list, activeId };
    });
  },

  setActive: (id) => set({ activeId: id }),
}));
