import { create } from 'zustand';
import { terminalStart, type TerminalStartInfo } from '../ipc';

interface TerminalEntry {
  terminalId: number;
  sessionId: string;
  command: string;
}

interface TerminalsStore {
  byConv: Record<string, TerminalEntry>;
  /** convId → exit code; presence of the key means exited. */
  exited: Record<string, number | null>;
  ensureStarted: (convId: string) => Promise<TerminalStartInfo>;
  markExited: (convId: string, code: number | null) => void;
  drop: (convId: string) => void;
}

export const useTerminalsStore = create<TerminalsStore>((set, get) => ({
  byConv: {},
  exited: {},

  ensureStarted: async (convId) => {
    const existing = get().byConv[convId];
    if (existing && !(convId in get().exited)) {
      return {
        conversationId: convId,
        terminalId: existing.terminalId,
        sessionId: existing.sessionId,
        command: existing.command,
        resumed: true,
      };
    }
    const info = await terminalStart(convId);
    set((s) => {
      const exited = { ...s.exited };
      delete exited[convId];
      return {
        byConv: {
          ...s.byConv,
          [convId]: {
            terminalId: info.terminalId,
            sessionId: info.sessionId,
            command: info.command,
          },
        },
        exited,
      };
    });
    return info;
  },

  markExited: (convId, code) =>
    set((s) => ({ exited: { ...s.exited, [convId]: code } })),

  drop: (convId) =>
    set((s) => {
      const byConv = { ...s.byConv };
      const exited = { ...s.exited };
      delete byConv[convId];
      delete exited[convId];
      return { byConv, exited };
    }),
}));
