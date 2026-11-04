import { useEffect, useState } from 'react';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import { onSessionUpdate, terminalKill } from './ipc';
import { useChatStore } from './stores/chat';
import { useConversationsStore } from './stores/conversations';
import { useSettingsStore } from './stores/settings';
import { useTerminalsStore } from './stores/terminals';
import ChatPanel from './components/ChatPanel';
import TerminalPanel from './components/TerminalPanel';
import SettingsPage from './components/SettingsPage';
import './App.css';

export default function App() {
  const activeId = useConversationsStore((s) => s.activeId);
  const loadConvs = useConversationsStore((s) => s.load);
  const loadSettings = useSettingsStore((s) => s.load);
  const [settingsOpen, setSettingsOpen] = useState(false);

  useEffect(() => {
    void loadSettings();
    void loadConvs();
    const un = onSessionUpdate((payload) => {
      useChatStore.getState().upsertFromSession(payload.conversationId, payload.upserts);
    });
    return () => {
      void un.then((f) => f());
    };
  }, [loadConvs, loadSettings]);

  // Draggable chat/terminal splitter.
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
      const ratio = (e.clientY - rect.top) / rect.height;
      const clamped = Math.min(Math.max(ratio, 0.15), 0.85);
      panes.style.setProperty('--chat-size', `${(clamped * 100).toFixed(1)}%`);
    };
    const onUp = () => { dragging = false; };
    el.addEventListener('mousedown', onDown);
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
    return () => {
      el.removeEventListener('mousedown', onDown);
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
  }, [activeId]);

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="sidebar-head">
          <span className="logo">YiTerm</span>
          <button className="icon-btn" title="设置" onClick={() => setSettingsOpen(true)}>
            ⚙
          </button>
        </div>
        <ConversationList />
      </aside>
      <main className="main">
        {activeId ? (
          <div className="panes">
            <div className="pane pane-chat">
              <ChatPanel convId={activeId} />
            </div>
            <div className="splitter" id="pane-splitter" />
            <div className="pane pane-term">
              <TerminalPanel convId={activeId} />
            </div>
          </div>
        ) : (
          <div className="empty">新建一个会话开始使用</div>
        )}
      </main>
      {settingsOpen && <SettingsPage onClose={() => setSettingsOpen(false)} />}
    </div>
  );
}

function ConversationList() {
  const { list, activeId, create, remove, setActive, loading } = useConversationsStore();
  const [creating, setCreating] = useState(false);

  const handleNew = async () => {
    try {
      const folder = await openDialog({ directory: true, multiple: false });
      if (typeof folder === 'string') {
        setCreating(true);
        await create(folder);
      }
    } finally {
      setCreating(false);
    }
  };

  return (
    <div className="conv-list">
      <button className="new-conv" onClick={handleNew} disabled={creating}>
        {creating ? '创建中…' : '＋ 新会话'}
      </button>
      {loading && <div className="conv-hint">加载中…</div>}
      {list.map((c) => (
        <div
          key={c.id}
          className={`conv-item ${c.id === activeId ? 'active' : ''}`}
          onClick={() => setActive(c.id)}
        >
          <div className="conv-title">{c.title ?? c.folder}</div>
          <div className="conv-folder" title={c.folder}>{c.folder}</div>
          <button
            className="conv-del"
            title="删除会话"
            onClick={async (e) => {
              e.stopPropagation();
              const term = useTerminalsStore.getState().byConv[c.id];
              if (term) void terminalKill(term.terminalId);
              useTerminalsStore.getState().drop(c.id);
              useChatStore.getState().clearConv(c.id);
              await remove(c.id);
            }}
          >
            ×
          </button>
        </div>
      ))}
      {!loading && list.length === 0 && (
        <div className="conv-hint">选择一个项目文件夹<br />创建第一个会话</div>
      )}
    </div>
  );
}
