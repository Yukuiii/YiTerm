import { useEffect } from 'react';

/**
 * Drag handler for the chat/terminal splitter: while dragging, the pane width
 * ratio is written to the `--chat-size` custom property on the split's parent,
 * clamped so neither pane can collapse.
 */
export function usePaneSplitter(enabled: boolean) {
  useEffect(() => {
    const el = document.getElementById('pane-splitter');
    if (!el) return;
    let dragging = false;
    const onDown = (e: MouseEvent) => {
      dragging = true;
      e.preventDefault();
    };
    const onMove = (e: MouseEvent) => {
      if (!dragging) return;
      const panes = el.parentElement;
      if (!panes) return;
      const rect = panes.getBoundingClientRect();
      const ratio = (e.clientX - rect.left) / rect.width;
      const clamped = Math.min(Math.max(ratio, 0.2), 0.8);
      panes.style.setProperty('--chat-size', `${(clamped * 100).toFixed(1)}%`);
    };
    const onUp = () => {
      dragging = false;
    };
    el.addEventListener('mousedown', onDown);
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
    return () => {
      el.removeEventListener('mousedown', onDown);
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
  }, [enabled]);
}
