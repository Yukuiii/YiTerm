import {
  Component,
  useEffect,
  useMemo,
  useState,
  type ErrorInfo,
  type ReactNode,
} from 'react';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import { revealItemInDir } from '@tauri-apps/plugin-opener';
import { appConfigDir, appLogDir } from '@tauri-apps/api/path';
import {
  currentUser,
  debugLog,
  onConversationBusy,
  onSessionUpdate,
  terminalKill,
  type Conversation,
} from './ipc';
import { useChatStore } from './stores/chat';
import { useConversationsStore } from './stores/conversations';
import { useSettingsStore } from './stores/settings';
import { useTerminalsStore } from './stores/terminals';
import { useSessionTitles } from './hooks/useSessionTitles';
import { folderName } from './format';
import type { ViewMode } from './types';
import Sidebar from './components/Sidebar';
import NewSessionView from './components/NewSessionView';
import ChatPanel from './components/ChatPanel';
import TerminalPanel from './components/TerminalPanel';
import SettingsPage from './components/SettingsPage';
import { FolderIcon, TerminalIcon } from './components/Icons';
import './App.css';

export default function App() {
  const activeId = useConversationsStore((s) => s.activeId);
  const conversations = useConversationsStore((s) => s.list);
  const projects = useConversationsStore((s) => s.projects);
  const loadConvs = useConversationsStore((s) => s.load);
  const createConv = useConversationsStore((s) => s.create);
  const removeConv = useConversationsStore((s) => s.remove);
  const setActive = useConversationsStore((s) => s.setActive);
  const settings = useSettingsStore((s) => s.settings);
  const loadSettings = useSettingsStore((s) => s.load);
  const titles = useSessionTitles();
  const terminalEntry = useTerminalsStore((s) => (activeId ? s.byConv[activeId] : undefined));
  const exitCode = useTerminalsStore((s) =>
    activeId ? (activeId in s.exited ? s.exited[activeId] : undefined) : undefined,
  );

  const [viewMode, setViewMode] = useState<ViewMode>(() => {
    const saved = localStorage.getItem('yiterm.viewMode');
    return saved === 'work' || saved === 'code' ? saved : 'code';
  });
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [newFolder, setNewFolder] = useState<string | null>(null);
  const [userName, setUserName] = useState('user');

  useEffect(() => {
    void loadSettings();
    void loadConvs();
    void currentUser().then(setUserName).catch(() => {});
    const un = onSessionUpdate((payload) => {
      // Imported history is shown as-is; only activity after attaching gets
      // the usual en→zh treatment.
      const translate = !(payload.imported && payload.historical);
      useChatStore
        .getState()
        .upsertFromSession(payload.conversationId, payload.upserts, translate);
    });
    // Sidebar spinner: a turn is running in that conversation's terminal.
    const unBusy = onConversationBusy((p) => {
      useTerminalsStore.getState().setBusy(p.conversationId, p.busy);
    });
    return () => {
      void un.then((f) => f());
      void unBusy.then((f) => f());
    };
  }, [loadConvs, loadSettings]);

  useEffect(() => {
    localStorage.setItem('yiterm.viewMode', viewMode);
  }, [viewMode]);

  // Claude Code can start sessions outside YiTerm (CLI, another window).
  // Re-scan while visible and whenever the window regains focus.
  useEffect(() => {
    const refresh = () => {
      if (document.visibilityState === 'visible') {
        void useConversationsStore.getState().refresh();
      }
    };
    const timer = window.setInterval(refresh, 5000);
    window.addEventListener('focus', refresh);
    document.addEventListener('visibilitychange', refresh);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener('focus', refresh);
      document.removeEventListener('visibilitychange', refresh);
    };
  }, []);

  // The terminal must run in both view modes (chat replies depend on it), so
  // start it here rather than only when TerminalPanel mounts.
  useEffect(() => {
    if (!activeId) return;
    void useTerminalsStore.getState().ensureStarted(activeId).catch(() => {});
  }, [activeId]);

  // Hydrate caches so sidebar labels can use the first user prompt. Imported
  // sessions only hydrate when opened (ChatPanel does it); most have no cache,
  // and the backend title is enough for the sidebar.
  useEffect(() => {
    for (const c of conversations) {
      if (!c.imported) void useChatStore.getState().hydrate(c.id);
    }
  }, [conversations]);

  const recentFolders = useMemo(() => {
    const seen = new Set<string>();
    const out: string[] = [];
    for (const c of conversations) {
      if (seen.has(c.folder)) continue;
      seen.add(c.folder);
      out.push(c.folder);
    }
    return out;
  }, [conversations]);

  const effectiveNewFolder: string | null = newFolder ?? recentFolders[0] ?? null;

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
  }, [activeId, viewMode]);

  const activeConv = conversations.find((c) => c.id === activeId) ?? null;
  const headerTitle = activeConv ? (titles[activeConv.id] ?? activeConv.title ?? '会话') : '新建会话';
  const statusFolder = activeConv?.folder ?? effectiveNewFolder;
  const translationLabel =
    settings?.translation.mode === 'chat'
      ? settings.translation.chatModelId
        ? `Chat · ${settings.translation.chatModelId}`
        : 'Chat 翻译'
      : settings?.translation.mode === 'local'
        ? '本地翻译 · Hy-MT2-1.8B'
        : settings?.translation.mode === 'deepl'
          ? 'DeepL 翻译'
          : settings?.translation.mode === 'dlx'
            ? 'DeepL 免费通道'
            : 'Google 翻译';
  const statusKind = !activeId
    ? 'idle'
    : exitCode !== undefined
      ? 'error'
      : terminalEntry
        ? 'ok'
        : 'idle';

  const pickFolder = async (): Promise<string | null> => {
    try {
      const folder = await openDialog({ directory: true, multiple: false });
      return typeof folder === 'string' ? folder : null;
    } catch {
      return null;
    }
  };

  const handleNew = async () => {
    const folder = await pickFolder();
    if (!folder) return;
    try {
      await createConv(folder);
    } catch (e) {
      console.error('create conversation failed:', e);
    }
  };

  const handleCreateInFolder = (folder: string) => {
    void createConv(folder).catch((e) => console.error('create conversation failed:', e));
  };

  const handleDelete = (conv: Conversation) => {
    const term = useTerminalsStore.getState().byConv[conv.id];
    if (term) void terminalKill(term.terminalId);
    useTerminalsStore.getState().drop(conv.id);
    useChatStore.getState().clearConv(conv.id);
    void removeConv(conv.id).catch((e) => console.error('delete conversation failed:', e));
  };

  const handlePickNewFolder = () => {
    void pickFolder().then((folder) => {
      if (folder) setNewFolder(folder);
    });
  };

  const handleStartEmpty = async () => {
    let folder: string | null = effectiveNewFolder;
    if (!folder) {
      folder = await pickFolder();
      if (folder) setNewFolder(folder);
    }
    if (!folder) return;
    try {
      await createConv(folder);
    } catch (e) {
      console.error('create conversation failed:', e);
    }
  };

  const handlePrompt = async (text: string) => {
    if (!effectiveNewFolder) throw new Error('请先选择一个项目文件夹');
    const conv = await createConv(effectiveNewFolder);
    useChatStore.getState().queuePrompt(conv.id, text);
  };

  const revealFolder = (folder: string) => {
    void revealItemInDir(folder).catch(() => {});
  };

  const handleRevealConfigDir = () => {
    void appConfigDir()
      .then((p) => revealItemInDir(p))
      .catch(() => {});
  };

  const handleRevealLogsDir = () => {
    void appLogDir()
      .then((p) => revealItemInDir(p))
      .catch(() => {});
  };

  return (
    <div className="app">
      <SidebarBoundary>
        <Sidebar
          conversations={conversations}
          projects={projects}
          activeId={activeId}
          titles={titles}
          mode={viewMode}
          userName={userName}
          onModeChange={setViewMode}
          onNew={() => void handleNew()}
          onOpenSettings={() => setSettingsOpen(true)}
          onCreateInFolder={handleCreateInFolder}
          onSelect={setActive}
          onDelete={handleDelete}
          onRevealConfig={handleRevealConfigDir}
          onRevealLogs={handleRevealLogsDir}
        />
      </SidebarBoundary>

      <main className="main">
        <header className="main-head" data-tauri-drag-region>
          <div className="main-title" data-tauri-drag-region>
            {headerTitle}
          </div>
          <div className="main-actions">
            <button
              className={`icon-btn ${viewMode === 'code' ? 'active' : ''}`}
              title={viewMode === 'code' ? '隐藏终端' : '显示终端'}
              onClick={() => setViewMode(viewMode === 'code' ? 'work' : 'code')}
            >
              <TerminalIcon size={17} />
            </button>
            <button
              className="icon-btn"
              title={activeConv ? '在访达中显示项目文件夹' : '选择项目文件夹'}
              onClick={() => {
                if (activeConv) revealFolder(activeConv.folder);
                else handlePickNewFolder();
              }}
            >
              <FolderIcon size={17} />
            </button>
          </div>
        </header>

        <div className="main-body">
          {activeId ? (
            <div className={`session ${viewMode}`}>
              <div className="pane pane-chat">
                <ChatPanel convId={activeId} />
              </div>
              {viewMode === 'code' && (
                <>
                  <div className="splitter" id="pane-splitter" />
                  <div className="pane pane-term">
                    <TerminalPanel convId={activeId} />
                  </div>
                </>
              )}
            </div>
          ) : (
            <NewSessionView
              folders={recentFolders}
              selectedFolder={effectiveNewFolder}
              onSelectFolder={setNewFolder}
              onPickFolder={handlePickNewFolder}
              onStartEmpty={handleStartEmpty}
              onOpenSettings={() => setSettingsOpen(true)}
              onPrompt={handlePrompt}
            />
          )}
        </div>

        <footer className="statusbar">
          <div className="status-left">
            <button
              className="status-folder-btn"
              title={statusFolder ?? '选择项目文件夹'}
              onClick={() => (statusFolder ? revealFolder(statusFolder) : handlePickNewFolder())}
            >
              <FolderIcon size={13} />
              <span className="status-folder">
                {statusFolder ? folderName(statusFolder) : '未选择项目'}
              </span>
            </button>
            {activeConv && (
              <span
                className="status-session"
                title={`当前 Claude 会话 ID：${activeConv.sessionId}（转录名 / --resume 用）`}
              >
                <span className="status-session-label">会话</span>
                <span className="status-session-id">{activeConv.sessionId}</span>
              </span>
            )}
          </div>
          <div className="status-right">
            <span className="status-item">{translationLabel}</span>
            <span className="status-item">Claude Code</span>
            <span
              className={`status-dot ${statusKind}`}
              title={
                statusKind === 'ok' ? '终端运行中' : statusKind === 'error' ? '终端已退出' : '终端未启动'
              }
            />
          </div>
        </footer>
      </main>

      {settingsOpen && <SettingsPage onClose={() => setSettingsOpen(false)} />}
    </div>
  );
}

/**
 * Keeps a failed sidebar from silently disappearing (and from expanding the
 * main pane): logs the stack to the backend log and offers a retry.
 */
class SidebarBoundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  state: { failed: boolean } = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    void debugLog(
      `sidebar render error: ${error.message}\n${info.componentStack ?? ''}\n${error.stack ?? ''}`,
    );
  }

  render() {
    if (this.state.failed) {
      return (
        <aside className="sidebar sidebar-failed">
          <div className="side-empty">侧边栏加载出错</div>
          <button className="new-session-btn" onClick={() => this.setState({ failed: false })}>
            重试
          </button>
        </aside>
      );
    }
    return this.props.children;
  }
}
