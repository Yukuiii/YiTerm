import { useCallback, useId, useState } from 'react';
import type { GitArea, GitFileChange } from '../ipc';
import type { GitStatusState } from '../hooks/useGitStatus';
import GitDiffDialog from './GitDiffDialog';
import FileTypeIcon from './FileTypeIcon';
import { ChevronIcon } from './Icons';

const STATUS_LABELS: Record<string, string> = {
  A: '新增', M: '修改', D: '删除', R: '重命名', C: '复制',
  T: '类型更改', U: '存在冲突', '?': '未跟踪的新文件',
};

interface Selection {
  area: GitArea;
  file: GitFileChange;
}

export default function GitChangesPanel({ folder, gitStatus }: { folder: string; gitStatus: GitStatusState }) {
  const [collapsed, setCollapsed] = useState(false);
  const { status, error, revision, refresh: refreshStatus } = gitStatus;
  const [selection, setSelection] = useState<Selection | null>(null);
  const bodyId = useId();
  const closeDialog = useCallback(() => setSelection(null), []);

  const total = status ? status.staged.length + status.unstaged.length : 0;
  const selectedFile = selection && status?.[selection.area].find(
    (file) => file.path === selection.file.path,
  );

  return (
    <section className={`git-panel${collapsed ? ' collapsed' : ''}`} aria-label="更改">
      <div className="git-panel-head">
        <button
          className="git-panel-toggle"
          aria-expanded={!collapsed}
          aria-controls={bodyId}
          title={status?.root ?? folder}
          onClick={() => setCollapsed((value) => !value)}
        >
          <ChevronIcon size={13} className={collapsed ? '' : 'expanded'} />
          <span>更改</span>
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
