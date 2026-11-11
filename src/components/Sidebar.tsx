import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from 'react';
import type { Conversation } from '../ipc';
import type { ViewMode } from '../types';
import { useTerminalsStore } from '../stores/terminals';
import { folderName, oneLine } from '../format';
import GitChangesPanel from './GitChangesPanel';
import GitGraphPanel from './GitGraphPanel';
import {
  ChatIcon,
  ChevronIcon,
  CodeIcon,
  FolderIcon,
  GearIcon,
  GitBranchIcon,
  MoreIcon,
  PlusIcon,
  SearchIcon,
  SlidersIcon,
  SpinnerIcon,
} from './Icons';

type SidebarView = 'sessions' | 'git';

interface SidebarProps {
  conversations: Conversation[];
  /** Registered Claude Code projects, including ones with no sessions yet. */
  projects: string[];
  activeId: string | null;
  titles: Record<string, string>;
  mode: ViewMode;
  userName: string;
  onModeChange: (mode: ViewMode) => void;
  onNew: () => void;
  onOpenSettings: () => void;
  onCreateInFolder: (folder: string) => void;
  onSelect: (id: string) => void;
  onRevealConfig: () => void;
  onRevealLogs: () => void;
}

export default function Sidebar({
  conversations,
  projects,
  activeId,
  titles,
  mode,
  userName,
  onModeChange,
  onNew,
  onOpenSettings,
  onCreateInFolder,
  onSelect,
  onRevealConfig,
  onRevealLogs,
}: SidebarProps) {
  const [view, setView] = useState<SidebarView>(() =>
    localStorage.getItem('yiterm.sidebarView') === 'git' ? 'git' : 'sessions',
  );
  const viewId = useId();
  const viewTabs = useRef<Partial<Record<SidebarView, HTMLButtonElement | null>>>({});
  useEffect(() => {
    localStorage.setItem('yiterm.sidebarView', view);
  }, [view]);
  const [searchOpen, setSearchOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [moreOpen, setMoreOpen] = useState(false);
  // Explicit user toggles win over the "active project is open" default.
  const [openOverrides, setOpenOverrides] = useState<Record<string, boolean>>({});
  const busyConv = useTerminalsStore((s) => s.busyConv);
  const q = query.trim().toLowerCase();
  const activeFolder = conversations.find((c) => c.id === activeId)?.folder;

  const groups = useMemo(() => {
    const map = new Map<string, Conversation[]>();
    for (const c of conversations) {
      const bucket = map.get(c.folder);
      if (bucket) bucket.push(c);
      else map.set(c.folder, [c]);
    }
    // Claude projects without sessions still get a group (with "+"), so every
    // project registered in ~/.claude.json is reachable from the sidebar.
    for (const folder of projects) {
      if (!map.has(folder)) map.set(folder, []);
    }
    return Array.from(map.entries()).map(([folder, convs]) => ({
      folder,
      name: folderName(folder),
      convs: convs.map((conv) => ({
        conv,
        label: titles[conv.id] ?? oneLine(conv.title || folderName(conv.folder), 48),
      })),
    }));
  }, [conversations, titles, projects]);

  const visibleGroups = useMemo(() => {
    if (!q) return groups;
    return groups
      .map((g) => {
        const groupHit = g.name.toLowerCase().includes(q) || g.folder.toLowerCase().includes(q);
        return {
          ...g,
          convs: groupHit ? g.convs : g.convs.filter((x) => x.label.toLowerCase().includes(q)),
        };
      })
      .filter((g) => g.convs.length > 0);
  }, [groups, q]);

  const toggleFolder = (folder: string, open: boolean) =>
    setOpenOverrides((prev) => ({ ...prev, [folder]: !open }));

  const closeSearch = () => {
    setSearchOpen(false);
    setQuery('');
  };

  const selectView = (next: SidebarView) => {
    setView(next);
    setMoreOpen(false);
    viewTabs.current[next]?.focus();
  };

  const onViewKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    let next: SidebarView;
    if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
      next = view === 'sessions' ? 'git' : 'sessions';
    } else if (event.key === 'Home') {
      next = 'sessions';
    } else if (event.key === 'End') {
      next = 'git';
    } else {
      return;
    }
    event.preventDefault();
    selectView(next);
  };

  return (
    <aside className="sidebar">
      <div className="sidebar-top" data-tauri-drag-region>
        <div className="mode-switch in-sidebar" role="tablist" aria-label="界面模式">
          <button
            className={`mode-btn ${mode === 'work' ? 'active' : ''}`}
            role="tab"
            aria-selected={mode === 'work'}
            onClick={() => onModeChange('work')}
            title="仅对话"
          >
            <ChatIcon size={14} />
            Work
          </button>
          <button
            className={`mode-btn ${mode === 'code' ? 'active' : ''}`}
            role="tab"
            aria-selected={mode === 'code'}
            onClick={() => onModeChange('code')}
            title="对话 + 终端"
          >
            <CodeIcon size={14} />
            Code
          </button>
        </div>
      </div>

      <div className="sidebar-view-switch" role="tablist" aria-label="侧边栏视图" onKeyDown={onViewKeyDown}>
        {(['sessions', 'git'] as const).map((tab) => (
          <button
            key={tab}
            ref={(element) => { viewTabs.current[tab] = element; }}
            id={`${viewId}-${tab}-tab`}
            className={`sidebar-view-tab${view === tab ? ' active' : ''}`}
            role="tab"
            aria-selected={view === tab}
            aria-controls={`${viewId}-${tab}-panel`}
            tabIndex={view === tab ? 0 : -1}
            onClick={() => selectView(tab)}
          >
            {tab === 'sessions' ? <ChatIcon size={14} /> : <GitBranchIcon size={14} />}
            <span>{tab === 'sessions' ? 'Sessions' : 'Git'}</span>
          </button>
        ))}
      </div>

      <div
        className="sidebar-body"
        id={`${viewId}-sessions-panel`}
        role="tabpanel"
        aria-labelledby={`${viewId}-sessions-tab`}
        hidden={view !== 'sessions'}
      >
        <button className="new-session-btn" onClick={onNew}>
          <PlusIcon size={16} />
          <span>新会话</span>
        </button>

        <nav className="side-nav">
          <button
            className={`side-nav-btn ${searchOpen ? 'active' : ''}`}
            onClick={() => (searchOpen ? closeSearch() : setSearchOpen(true))}
          >
            <SearchIcon size={16} />
            <span>搜索</span>
          </button>
          <button className="side-nav-btn" onClick={onOpenSettings}>
            <SlidersIcon size={16} />
            <span>自定义</span>
          </button>
          <div className="side-nav-more">
            <button
              className={`side-nav-btn ${moreOpen ? 'active' : ''}`}
              onClick={() => setMoreOpen((v) => !v)}
            >
              <MoreIcon size={16} />
              <span>更多</span>
            </button>
            {moreOpen && (
              <div className="more-menu">
                <button
                  onClick={() => {
                    setMoreOpen(false);
                    onRevealConfig();
                  }}
                >
                  <FolderIcon size={14} />
                  打开配置目录
                </button>
                <button
                  onClick={() => {
                    setMoreOpen(false);
                    onRevealLogs();
                  }}
                >
                  <FolderIcon size={14} />
                  打开日志目录
                </button>
              </div>
            )}
          </div>
        </nav>

        {searchOpen && (
          <input
            className="side-search"
            autoFocus
            value={query}
            placeholder="搜索会话…"
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Escape') closeSearch();
            }}
          />
        )}

        <div className="side-list">
          {visibleGroups.map((g) => {
            const open = q ? true : (openOverrides[g.folder] ?? g.folder === activeFolder);
            return (
              <div className="project" key={g.folder}>
                <div className="project-row">
                  <button
                    className="project-toggle"
                    aria-expanded={open}
                    onClick={() => toggleFolder(g.folder, open)}
                    title={g.folder}
                  >
                    <span className="project-icon" aria-hidden="true">
                      <FolderIcon size={14} />
                      <ChevronIcon size={13} className={`chevron ${open ? 'open' : ''}`} />
                    </span>
                    <span className="project-name">{g.name}</span>
                  </button>
                  <button
                    className="project-add"
                    title={`在 ${g.name} 中新建会话`}
                    onClick={() => onCreateInFolder(g.folder)}
                  >
                    <PlusIcon size={14} />
                  </button>
                </div>
                {open && (
                  <div className="sessions">
                    {g.convs.length === 0 && <div className="session-empty">暂无会话</div>}
                    {g.convs.map(({ conv, label }) => (
                      <div
                        key={conv.id}
                        className={`session-row ${conv.id === activeId ? 'active' : ''}`}
                        role="button"
                        tabIndex={0}
                        title={conv.folder}
                        onClick={() => onSelect(conv.id)}
                        onKeyDown={(e) => {
                          if (e.key === 'Enter' || e.key === ' ') {
                            e.preventDefault();
                            onSelect(conv.id);
                          }
                        }}
                      >
                        {busyConv[conv.id] && (
                          <span className="session-busy" title="运行中" aria-label="运行中">
                            <SpinnerIcon size={12} />
                          </span>
                        )}
                        <span className="session-title">{label}</span>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            );
          })}

          {q && visibleGroups.length === 0 && <div className="side-empty">没有匹配的会话</div>}
          {!q && visibleGroups.length === 0 && (
            <div className="side-empty">
              选择项目文件夹
              <br />
              创建第一个会话
            </div>
          )}
        </div>
      </div>

      <div
        className="sidebar-body sidebar-review"
        id={`${viewId}-git-panel`}
        role="tabpanel"
        aria-labelledby={`${viewId}-git-tab`}
        hidden={view !== 'git'}
      >
        {view === 'git' && (activeFolder ? (
          <div className="git-workspace" key={activeFolder}>
            <GitChangesPanel folder={activeFolder} />
            <GitGraphPanel folder={activeFolder} />
          </div>
        ) : (
          <div className="sidebar-review-empty">
            <p>选择一个会话以查看项目更改。</p>
            <button className="sidebar-view-return" onClick={() => selectView('sessions')}>Sessions</button>
          </div>
        ))}
      </div>

      <div className="sidebar-foot">
        <div className="identity">
          <div className="avatar">{(userName[0] || 'u').toUpperCase()}</div>
          <div className="identity-text">
            <div className="identity-name">{userName}</div>
            <div className="identity-sub">Claude Code</div>
          </div>
        </div>
        <button className="icon-btn" title="设置" onClick={onOpenSettings}>
          <GearIcon size={16} />
        </button>
      </div>
    </aside>
  );
}
