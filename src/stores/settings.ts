import { create } from 'zustand';
import { settingsGet, settingsSet, type Settings } from '../ipc';

interface SettingsStore {
  settings: Settings | null;
  loaded: boolean;
  load: () => Promise<void>;
  save: (next: Settings) => Promise<void>;
}

export const useSettingsStore = create<SettingsStore>((set) => ({
  settings: null,
  loaded: false,

  load: async () => {
    const settings = await settingsGet();
    set({ settings, loaded: true });
  },

  save: async (next) => {
    await settingsSet(next);
    set({ settings: next });
  },
}));
