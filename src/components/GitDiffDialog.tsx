import { useEffect, useId, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { gitDiff, type GitArea, type GitDiff, type GitFileChange } from '../ipc';
import { SpinnerIcon, XIcon } from './Icons';

const MAX_VISIBLE_LINES = 6000;
const FOCUSABLE = 'button:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])';

function lineClass(line: string, combined: boolean): string {
  if (line.startsWith('@@')) return 'git-diff-hunk';
  if (/^(diff |index |--- |\+\+\+ |new file |deleted file |.* mode |rename |copy |similarity |dissimilarity |Binary files )/.test(line)) {
    return 'git-diff-meta';
  }
  // Combined diffs have one prefix column per parent; do not interpret them
  // as ordinary single-parent additions/deletions.
  if (!combined && line.startsWith('+')) return 'git-diff-add';
  if (!combined && line.startsWith('-')) return 'git-diff-delete';
  return '';
}

export default function GitDiffDialog({
  folder, area, file, present, revision, onClose, onRefreshStatus,
}: {
  folder: string;
  area: GitArea;
  file: GitFileChange;
  present: boolean;
  revision: number;
  onClose: () => void;
  onRefreshStatus: () => void;
}) {
  const [snapshot, setDiff] = useState<GitDiff | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const diff = present ? snapshot : null;
  const displayError = present ? error : '该文件已不在所选更改列表中，可能已暂存、提交或还原。';
  const isLoading = present && loading;
  const cardRef = useRef<HTMLDivElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);
  const refreshRef = useRef<() => void>(() => {});
  const titleId = useId();

  useEffect(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const card = cardRef.current!;
    closeRef.current?.focus();
    const focusable = () => Array.from(card.querySelectorAll<HTMLElement>(FOCUSABLE))
      .filter((element) => element.getClientRects().length > 0);
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && !event.isComposing) {
        // Capture before ChatPanel's document listener can interrupt the PTY.
        event.preventDefault();
        event.stopPropagation();
        onClose();
      } else if (event.key === 'Tab') {
        const elements = focusable();
        const first = elements[0] ?? card;
        const last = elements[elements.length - 1] ?? card;
        if (event.shiftKey && (document.activeElement === first || document.activeElement === card)) {
          event.preventDefault();
          last.focus();
        } else if (!event.shiftKey && (document.activeElement === last || document.activeElement === card)) {
          event.preventDefault();
          first.focus();
        }
      }
    };
    const onFocus = (event: FocusEvent) => {
      if (event.target instanceof Node && !card.contains(event.target)) closeRef.current?.focus();
    };
    document.addEventListener('keydown', onKeyDown, true);
    document.addEventListener('focusin', onFocus, true);
    return () => {
      document.removeEventListener('keydown', onKeyDown, true);
      document.removeEventListener('focusin', onFocus, true);
      if (opener?.isConnected) opener.focus();
      else document.querySelector<HTMLButtonElement>('.main-actions button')?.focus();
    };
  }, [onClose]);

  useEffect(() => {
    const request = { disposed: false, running: false, queued: false };
    if (!present) {
      refreshRef.current = () => {};
      return;
    }
    const refresh = async () => {
      if (request.disposed) return;
      if (request.running) {
        request.queued = true;
        return;
      }
      request.running = true;
      setLoading(true);
      setError(null);
      do {
        request.queued = false;
        try {
          const next = await gitDiff(folder, area, file.path);
          if (!request.disposed && !request.queued) {
            setDiff(next);
            setError(null);
          }
        } catch (e) {
          if (!request.disposed && !request.queued) setError(String(e));
        }
      } while (request.queued && !request.disposed);
      request.running = false;
      if (!request.disposed) setLoading(false);
    };
    refreshRef.current = () => void refresh();
    return () => {
      request.disposed = true;
      refreshRef.current = () => {};
    };
  }, [folder, area, file.path, present]);

  useEffect(() => {
    refreshRef.current();
  }, [revision, folder, area, file.path, present]);

  const lines = useMemo(() => {
    const all = diff?.text.split('\n') ?? [];
    if (all[all.length - 1] === '') all.pop();
    return { visible: all.slice(0, MAX_VISIBLE_LINES), truncated: all.length > MAX_VISIBLE_LINES };
  }, [diff?.text]);
  const conflicted = file.conflicted || diff?.kind === 'conflict';

  return createPortal(
    <div
      className="settings-overlay git-diff-overlay"
      onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}
    >
      <div
        className="git-diff-card"
        ref={cardRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
      >
        <div className="git-diff-head">
          <div className="git-diff-heading">
            <div className="git-diff-title-row">
              <h2 id={titleId}>文件差异</h2>
              <span className="git-area-label">{area === 'staged' ? '已暂存' : '未暂存'}</span>
              {isLoading && <span className="git-spinner" aria-label="正在刷新差异"><SpinnerIcon size={13} /></span>}
            </div>
            <div className="git-diff-path" title={file.path}>{file.path}</div>
            {file.originalPath && <div className="git-diff-previous">原路径：{file.originalPath}</div>}
          </div>
          <div className="git-diff-actions">
            <button
              className="git-refresh"
              disabled={isLoading}
              onClick={() => present ? refreshRef.current() : onRefreshStatus()}
            >刷新</button>
            <button className="icon-btn" ref={closeRef} onClick={onClose} aria-label="关闭差异窗口">
              <XIcon size={17} />
            </button>
          </div>
        </div>
        <div className="git-diff-comparison">
          {area === 'staged' ? 'HEAD → 暂存区' : '暂存区 → 工作区'}
        </div>
        {displayError && <div className="git-error" role="alert">{displayError}</div>}
        {conflicted && <div className="git-diff-notice">文件存在合并冲突；以下保留 Git 的合并差异格式。</div>}
        {diff?.kind === 'binary' && <div className="git-diff-notice">二进制文件已更改，无法显示文本差异。</div>}
        {(diff?.truncated || lines.truncated) && (
          <div className="git-diff-notice">差异较大，已显示部分内容。完整差异可在终端中查看。</div>
        )}
        {diff?.text ? (
          <pre className="git-diff-content" tabIndex={0} aria-label="文件差异内容" aria-busy={isLoading}>
            {lines.visible.map((line, index) => (
              <span className={`git-diff-line ${lineClass(line, conflicted)}`} key={index}>{line || ' '}</span>
            ))}
          </pre>
        ) : (
          <div className="git-diff-placeholder" role="status">
            {isLoading ? '正在加载差异…' : displayError ? '请刷新或选择其他文件。' : '没有可显示的文本差异。'}
          </div>
        )}
      </div>
    </div>,
    document.body,
  );
}
