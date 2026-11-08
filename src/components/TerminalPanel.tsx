import { useEffect, useRef, useState } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { Channel } from '@tauri-apps/api/core';
import {
  decodeBase64ToBytes,
  terminalAttach,
  terminalResize,
  terminalWrite,
  type TerminalEvent,
} from '../ipc';
import { useTerminalsStore } from '../stores/terminals';
import { useSettingsStore } from '../stores/settings';
import '@xterm/xterm/css/xterm.css';

const DEFAULT_FONT_SIZE = 13;
const DEFAULT_LINE_HEIGHT = 1.1;
const BUNDLED_FONT = "'Ioskeley Mono Term', ui-monospace, SFMono-Regular, Menlo, monospace";

function fontStack(family?: string | null): string {
  const custom = family?.replace(/["']/g, '').trim();
  return custom
    ? `"${custom}", ui-monospace, SFMono-Regular, Menlo, monospace`
    : BUNDLED_FONT;
}

function applyTermOptions(
  term: Terminal,
  fontFamily?: string | null,
  fontSize?: number,
  lineHeight?: number,
) {
  term.options.fontFamily = fontStack(fontFamily);
  term.options.fontSize = Math.min(32, Math.max(8, fontSize ?? DEFAULT_FONT_SIZE));
  term.options.lineHeight = Math.min(3, Math.max(0.8, lineHeight ?? DEFAULT_LINE_HEIGHT));
}

export default function TerminalPanel({ convId }: { convId: string }) {
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const terminalIdRef = useRef<number | null>(null);
  const termSettings = useSettingsStore((s) => s.settings?.terminal);
  const fontFamily = termSettings?.fontFamily;
  const fontSize = termSettings?.fontSize;
  const lineHeight = termSettings?.lineHeight;
  // Latest settings for the initial terminal construction (the settings store
  // may load after this component mounts).
  const settingsRef = useRef(termSettings);
  settingsRef.current = termSettings;
  const [attempt, setAttempt] = useState(0);
  const ensureStarted = useTerminalsStore((s) => s.ensureStarted);
  const markExited = useTerminalsStore((s) => s.markExited);
  const exited = useTerminalsStore((s) => (convId in s.exited ? s.exited[convId] : undefined));

  const restart = () => {
    useTerminalsStore.getState().drop(convId);
    terminalIdRef.current = null;
    setAttempt((a) => a + 1);
  };

  useEffect(() => {
    let disposed = false;
    let resizeTimer: number | undefined;
    let observer: ResizeObserver | null = null;
    let term: Terminal | null = null;
    let fit: FitAddon | null = null;
    const ts = settingsRef.current;

    (async () => {
      // 1. Ensure a live terminal + follower exist for this conversation.
      const info = await ensureStarted(convId).catch((e) => {
        console.error('terminal_start failed:', e);
        return null;
      });
      if (!info || disposed) return;
      terminalIdRef.current = info.terminalId;

      // 2. Wait for the bundled terminal font, otherwise xterm measures the
      // fallback face and the glyph cells come out the wrong size. A custom
      // family is system-installed, so there is nothing to load.
      if (!ts?.fontFamily?.trim()) {
        await Promise.all([
          document.fonts.load('400 13px "Ioskeley Mono Term"'),
          document.fonts.load('700 13px "Ioskeley Mono Term"'),
          document.fonts.load('italic 400 13px "Ioskeley Mono Term"'),
          document.fonts.load('italic 700 13px "Ioskeley Mono Term"'),
        ]).catch(() => {});
      }
      if (disposed) return;

      // 3. Build the xterm view.
      term = new Terminal({
        cursorBlink: true,
        fontSize: 12.5,
        fontFamily: "'Ioskeley Mono Term', ui-monospace, SFMono-Regular, Menlo, monospace",
        theme: {
          background: '#0c0c0c',
          foreground: '#d7d7d7',
          cursor: '#d7d7d7',
          selectionBackground: 'rgba(255, 255, 255, 0.18)',
        },
        scrollback: 5000,
      });
      fit = new FitAddon();
      term.loadAddon(fit);
      term.open(hostRef.current!);
      termRef.current = term;
      fitRef.current = fit;
      // Apply the latest typography after construction too: the settings store
      // can update while this async effect is still starting the PTY.
      const latest = settingsRef.current;
      applyTermOptions(term, latest?.fontFamily, latest?.fontSize, latest?.lineHeight);
      try { fit.fit(); } catch { /* host not sized yet */ }
      void terminalResize(info.terminalId, term.cols, term.rows);

      // 4. Keystrokes → PTY.
      term.onData((data) => {
        const id = terminalIdRef.current;
        if (id !== null) void terminalWrite(id, data, false);
      });

      // 5. Backlog replay + live stream.
      const channel = new Channel<TerminalEvent>();
      channel.onmessage = (ev) => {
        if (ev.type === 'data') {
          if (ev.reset) term!.reset();
          term!.write(decodeBase64ToBytes(ev.b64));
        } else if (ev.type === 'exit') {
          markExited(convId, ev.code);
        }
      };
      await terminalAttach(info.terminalId, 0, channel);

      // 6. ResizeObserver → debounced fit → PTY resize.
      observer = new ResizeObserver(() => {
        if (resizeTimer !== undefined) window.clearTimeout(resizeTimer);
        resizeTimer = window.setTimeout(() => {
          const id = terminalIdRef.current;
          if (!fit || !term || id === null) return;
          try {
            fit.fit();
            void terminalResize(id, term.cols, term.rows);
          } catch { /* not visible */ }
        }, 100);
      });
      observer.observe(hostRef.current!);
    })();

    return () => {
      disposed = true;
      if (resizeTimer !== undefined) window.clearTimeout(resizeTimer);
      observer?.disconnect();
      term?.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
    // convId / attempt change → full remount. ensureStarted/markExited are stable.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [convId, attempt]);

  // Apply font/size/line-height changes without restarting the PTY.
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    applyTermOptions(term, fontFamily, fontSize, lineHeight);
    try {
      fitRef.current?.fit();
    } catch {
      /* host not sized yet */
    }
    const id = terminalIdRef.current;
    if (id !== null) void terminalResize(id, term.cols, term.rows);
  }, [fontFamily, fontSize, lineHeight]);

  return (
    <div className="term-panel">
      <div className="term-host" ref={hostRef} />
      {exited !== undefined && (
        <button className="restart-btn term-exit" onClick={restart} title="用 --resume 恢复此会话">
          终端已退出{exited !== null ? ` (${exited})` : ''} — 点击恢复
        </button>
      )}
    </div>
  );
}
