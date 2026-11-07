import { useShallow } from 'zustand/react/shallow';
import { useChatStore } from '../stores/chat';
import { oneLine } from '../format';

/**
 * Conversation id → first user prompt (one line). Used for session labels in
 * the sidebar and the main header; falls back to the stored title when the
 * conversation has no prompt yet.
 */
export function useSessionTitles(): Record<string, string> {
  return useChatStore(
    useShallow((s) => {
      const out: Record<string, string> = {};
      for (const [id, msgs] of Object.entries(s.byConv)) {
        const first = msgs.find((m) => m.role === 'user' && m.original.trim());
        if (first) out[id] = oneLine(first.original, 64);
      }
      return out;
    }),
  );
}
