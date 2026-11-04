import { create } from 'zustand';
import { apiKeyGet, apiKeySet, settingsGet, settingsSet, type Settings } from '../ipc';

interface SettingsStore {
  settings: Settings | null;
  hasApiKey: boolean;
  loaded: boolean;
  load: () => Promise<void>;
  save: (next: Settings) => Promise<void>;
  setApiKey: (key: string | null) => Promise<void>;
}

export const useSettingsStore = create<SettingsStore>((set) => ({
  settings: null,
  hasApiKey: false,
  loaded: false,

  load: async () => {
    const [settings, key] = await Promise.all([settingsGet(), apiKeyGet()]);
    set({ settings, hasApiKey: !!key, loaded: true });
  },

  save: async (next) => {
    await settingsSet(next);
    set({ settings: next });
  },

  setApiKey: async (key) => {
    await apiKeySet(key);
    set({ hasApiKey: !!key });
  },
}));
