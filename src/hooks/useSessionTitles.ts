import { useShallow } from 'zustand/react/shallow';
import { useChatStore } from '../stores/chat';
import { oneLine, stripImageMarkers } from '../format';

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
        // Image-only turns have no text; label them rather than leaving the
        // key absent and the sidebar row blank.
        const first = msgs.find(
          (m) => m.role === 'user' && (m.original.trim() || m.images?.length || m.hadImages),
        );
        if (first) {
          // Restored image turns keep Claude Code's `[Image #N]` marker in the
          // text; it is noise in a title.
          out[id] = oneLine(stripImageMarkers(first.original) || '[图片]', 64);
        }
      }
      return out;
    }),
  );
}
