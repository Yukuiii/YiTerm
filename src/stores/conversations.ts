import { create } from 'zustand';
import {
  conversationsCreate,
  conversationsDelete,
  conversationsList,
  type Conversation,
} from '../ipc';

interface ConversationsStore {
  list: Conversation[];
  activeId: string | null;
  loading: boolean;
  load: () => Promise<void>;
  create: (folder: string) => Promise<Conversation>;
  remove: (id: string) => Promise<void>;
  setActive: (id: string | null) => void;
}

export const useConversationsStore = create<ConversationsStore>((set, get) => ({
  list: [],
  activeId: null,
  loading: false,

  load: async () => {
    set({ loading: true });
    const list = await conversationsList();
    set({ list, loading: false });
    if (!get().activeId && list.length > 0) {
      set({ activeId: list[0].id });
    }
  },

  create: async (folder) => {
    const conv = await conversationsCreate(folder);
    set((s) => ({ list: [conv, ...s.list], activeId: conv.id }));
    return conv;
  },

  remove: async (id) => {
    await conversationsDelete(id);
    set((s) => {
      const list = s.list.filter((c) => c.id !== id);
      const activeId = s.activeId === id ? (list[0]?.id ?? null) : s.activeId;
      return { list, activeId };
    });
  },

  setActive: (id) => set({ activeId: id }),
}));
