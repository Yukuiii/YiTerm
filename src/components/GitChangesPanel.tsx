import { useCallback, useEffect, useId, useRef, useState } from 'react';
import { gitStatus, type GitArea, type GitFileChange, type GitStatus } from '../ipc';
import GitDiffDialog from './GitDiffDialog';
import FileTypeIcon from './FileTypeIcon';
import { ChevronIcon, GitBranchIcon } from './Icons';

const STATUS_LABELS: Record<string, string> = {
  A: '新增', M: '修改', D: '删除', R: '重命名', C: '复制',
  T: '类型更改', U: '存在冲突', '?': '未跟踪的新文件',
};

interface Selection {
  area: GitArea;
  file: GitFileChange;
}

export default function GitChangesPanel({ folder }: { folder: string }) {
  const [collapsed, setCollapsed] = useState(false);
  const [status, setStatus] = useState<GitStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const [selection, setSelection] = useState<Selection | null>(null);
  const expandedRef = useRef(true);
  const refreshRef = useRef<() => void>(() => {});
  const bodyId = useId();
  const closeDialog = useCallback(() => setSelection(null), []);
  const refreshStatus = useCallback(() => refreshRef.current(), []);

  // App keys this component by folder. Cleanup also protects against replies
  // arriving after switching workspaces or leaving code mode.
  useEffect(() => {
    const request = { disposed: false, running: false };
    const refresh = async () => {
      if (request.disposed || request.running || document.visibilityState !== 'visible') return;
      request.running = true;
      try {
        const next = await gitStatus(folder);
        if (!request.disposed) {
          setStatus(next);
          setError(null);
          // XY can stay the same while the actual file contents change.
          setRevision((value) => value + 1);
        }
      } catch (e) {
        if (!request.disposed) setError(String(e));
      } finally {
        request.running = false;
      }
    };
    const refreshVisible = () => {
      if (expandedRef.current) void refresh();
    };
    refreshRef.current = () => void refresh();
    refreshVisible();
    const timer = window.setInterval(refreshVisible, 5000);
    window.addEventListener('focus', refreshVisible);
    document.addEventListener('visibilitychange', refreshVisible);
    return () => {
      request.disposed = true;
      refreshRef.current = () => {};
      window.clearInterval(timer);
      window.removeEventListener('focus', refreshVisible);
      document.removeEventListener('visibilitychange', refreshVisible);
    };
  }, [folder]);

  useEffect(() => {
    expandedRef.current = !collapsed;
    if (!collapsed) refreshRef.current();
  }, [collapsed]);

  const total = status ? status.staged.length + status.unstaged.length : 0;
  const branch = status?.branch ?? 'Detached HEAD';
  const selectedFile = selection && status?.[selection.area].find(
    (file) => file.path === selection.file.path,
  );

  return (
    <section className={`git-panel${collapsed ? ' collapsed' : ''}`} aria-label="Document Review">
      <div className="git-panel-head">
        <button
          className="git-panel-toggle"
          aria-expanded={!collapsed}
          aria-controls={bodyId}
          title={status?.root ?? folder}
          onClick={() => setCollapsed((value) => !value)}
        >
          <ChevronIcon size={13} className={collapsed ? '' : 'expanded'} />
          <span>Document Review</span>
          {status?.root && <span className="git-count">{total}</span>}
        </button>
      </div>
      <div id={bodyId} className="git-panel-body" hidden={collapsed}>
        {error && <div className="git-error" role="alert">{error}</div>}
        {!status ? (
          !error && <div className="git-empty" role="status">正在读取文件更改…</div>
        ) : !status.root ? (
          <div className="git-empty">当前项目不在 Git 仓库中</div>
        ) : (
          <div className="git-columns">
            {(['unstaged', 'staged'] as const).map((area) => (
              <div className="git-group" key={area}>
                <h3>{area === 'staged' ? '已暂存' : '未暂存'}<span>{status[area].length}</span></h3>
                <div className="git-file-list">
                  {status[area].length === 0 && <div className="git-empty">暂无更改</div>}
                  {status[area].map((file) => {
                    const name = file.path.split('/').pop() || file.path;
                    const directory = file.path.slice(0, -name.length).replace(/\/$/, '');
                    const label = STATUS_LABELS[file.status] ?? '更改';
                    return (
                      <button
                        className="git-file"
                        key={file.path}
                        title={file.originalPath ? `${file.originalPath} → ${file.path}` : file.path}
                        aria-label={`${file.path}，${label}，查看${area === 'staged' ? '已暂存' : '未暂存'}差异`}
                        onClick={() => setSelection({ area, file })}
                      >
                        <FileTypeIcon path={file.path} />
                        <span className="git-file-path">
                          <span className="git-file-name">{name}</span>
                          {directory && <span className="git-file-directory">{directory}</span>}
                        </span>
                        <span className="git-file-status" data-status={file.status} title={label}>
                          {file.status}
                        </span>
                      </button>
                    );
                  })}
                </div>
              </div>
            ))}
          </div>
        )}
        {status?.root && (
          <div className="git-panel-branch" role="status" aria-label={`当前分支：${branch}`} title={branch}>
            <GitBranchIcon size={14} />
            <span>{branch}</span>
          </div>
        )}
      </div>
      {selection && (
        <GitDiffDialog
          key={`${selection.area}:${selection.file.path}`}
          folder={folder}
          area={selection.area}
          file={selectedFile || selection.file}
          present={!!selectedFile}
          revision={revision}
          onClose={closeDialog}
          onRefreshStatus={refreshStatus}
        />
      )}
    </section>
  );
}
