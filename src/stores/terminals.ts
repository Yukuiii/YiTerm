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
  /** convId → a turn is running (from the transcript follower). */
  busyConv: Record<string, boolean>;
  setBusy: (convId: string, busy: boolean) => void;
  ensureStarted: (convId: string) => Promise<TerminalStartInfo>;
  markExited: (convId: string, code: number | null) => void;
  drop: (convId: string) => void;
}

// In-flight dedup: React StrictMode double-invokes effects in dev, and two
// concurrent terminal_start calls would race past each other on the backend
// and spawn duplicate PTYs. One promise per conversation.
const starting = new Map<string, Promise<TerminalStartInfo>>();

export const useTerminalsStore = create<TerminalsStore>((set, get) => ({
  byConv: {},
  exited: {},
  busyConv: {},

  setBusy: (convId, busy) =>
    set((s) => {
      const busyConv = { ...s.busyConv };
      if (busy) busyConv[convId] = true;
      else delete busyConv[convId];
      return { busyConv };
    }),

  ensureStarted: async (convId) => {
    const existing = get().byConv[convId];
    if (existing && !(convId in get().exited)) {
      return {
        conversationId: convId,
        terminalId: existing.terminalId,
        sessionId: existing.sessionId,
        command: existing.command,
        resumed: true,
        evictedConversationIds: [],
      };
    }
    const pending = starting.get(convId);
    if (pending) return pending;
    const promise = terminalStart(convId)
      .finally(() => starting.delete(convId));
    starting.set(convId, promise);
    const info = await promise;
    set((s) => {
      const byConv = { ...s.byConv };
      const exited = { ...s.exited };
      const busyConv = { ...s.busyConv };
      delete exited[convId];
      byConv[convId] = {
        terminalId: info.terminalId,
        sessionId: info.sessionId,
        command: info.command,
      };
      // The backend reclaimed idle terminals to stay under its cap; forget
      // them here so visiting those conversations starts a fresh --resume.
      for (const id of info.evictedConversationIds) {
        if (id === convId) continue;
        delete byConv[id];
        delete exited[id];
        delete busyConv[id];
      }
      return { byConv, exited, busyConv };
    });
    return info;
  },

  markExited: (convId, code) =>
    set((s) => ({ exited: { ...s.exited, [convId]: code } })),

  drop: (convId) =>
    set((s) => {
      const byConv = { ...s.byConv };
      const exited = { ...s.exited };
      const busyConv = { ...s.busyConv };
      delete byConv[convId];
      delete exited[convId];
      delete busyConv[convId];
      return { byConv, exited, busyConv };
    }),
}));
