import { useMemo, useState } from 'react';
import type { Conversation } from '../ipc';
import type { ViewMode } from '../types';
import { folderName, oneLine } from '../format';
import {
  ChatIcon,
  ChevronIcon,
  CodeIcon,
  FolderIcon,
  GearIcon,
  MoreIcon,
  PlusIcon,
  SearchIcon,
  SlidersIcon,
  XIcon,
} from './Icons';

interface SidebarProps {
  conversations: Conversation[];
  activeId: string | null;
  titles: Record<string, string>;
  mode: ViewMode;
  userName: string;
  onModeChange: (mode: ViewMode) => void;
  onNew: () => void;
  onOpenSettings: () => void;
  onCreateInFolder: (folder: string) => void;
  onSelect: (id: string) => void;
  onDelete: (conv: Conversation) => void;
  onRevealConfig: () => void;
  onRevealLogs: () => void;
}

export default function Sidebar({
  conversations,
  activeId,
  titles,
  mode,
  userName,
  onModeChange,
  onNew,
  onOpenSettings,
  onCreateInFolder,
  onSelect,
  onDelete,
  onRevealConfig,
  onRevealLogs,
}: SidebarProps) {
  const [searchOpen, setSearchOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [moreOpen, setMoreOpen] = useState(false);
  // Explicit user toggles win over the "active project is open" default.
  const [openOverrides, setOpenOverrides] = useState<Record<string, boolean>>({});
  const q = query.trim().toLowerCase();
  const activeFolder = conversations.find((c) => c.id === activeId)?.folder;

  const groups = useMemo(() => {
    const map = new Map<string, Conversation[]>();
    for (const c of conversations) {
      const bucket = map.get(c.folder);
      if (bucket) bucket.push(c);
      else map.set(c.folder, [c]);
    }
    return Array.from(map.entries()).map(([folder, convs]) => ({
      folder,
      name: folderName(folder),
      convs: convs.map((conv) => ({
        conv,
        label: titles[conv.id] ?? oneLine(conv.title || folderName(conv.folder), 48),
      })),
    }));
  }, [conversations, titles]);

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

  return (
    <aside className="sidebar">
      <div className="sidebar-top" data-tauri-drag-region>
        <div className="mode-switch" role="tablist" aria-label="界面模式">
          <button
            className={`mode-btn ${mode === 'cowork' ? 'active' : ''}`}
            role="tab"
            aria-selected={mode === 'cowork'}
            onClick={() => onModeChange('cowork')}
            title="仅对话"
          >
            <ChatIcon size={14} />
            Cowork
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

      <div className="sidebar-body">
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
                  <button className="project-toggle" onClick={() => toggleFolder(g.folder, open)} title={g.folder}>
                    <ChevronIcon size={13} className={`chevron ${open ? 'open' : ''}`} />
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
                        <span className="session-title">{label}</span>
                        <button
                          className="session-del"
                          title="删除会话"
                          onClick={(e) => {
                            e.stopPropagation();
                            onDelete(conv);
                          }}
                        >
                          <XIcon size={13} />
                        </button>
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
