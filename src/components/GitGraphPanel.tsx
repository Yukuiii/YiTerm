import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent } from 'react';
import { createPortal } from 'react-dom';
import { gitCommitDetails, gitHistory, type GitCommit, type GitRef } from '../ipc';
import { createCommitDetailsCache, formatCommitDate } from '../gitCommitDetails';
import { GRAPH_ROW_HEIGHT, graphPath, graphX, layoutGitGraph } from '../gitGraph';
import { createGitHistoryLoader, HISTORY_LIMIT, type HistoryState } from '../gitHistory';
import GitCommitCard from './GitCommitCard';
import { ChevronIcon, RefreshIcon, SpinnerIcon } from './Icons';

function commitDescription(commit: GitCommit, refs: GitRef[], isHead: boolean) {
  return [
    commit.subject || '（无标题）',
    `${commit.authorName} · ${formatCommitDate(commit.authorTime)}`,
    commit.oid,
    isHead ? 'HEAD' : '',
    refs.map((ref) => ref.shortName).join('、'),
    commit.parents.length ? `父提交：${commit.parents.join('、')}` : '根提交',
  ].filter(Boolean).join('\n');
}

interface Detail {
  oid: string;
  left: number;
  top: number;
}

export default function GitGraphPanel({ folder }: { folder: string }) {
  const [collapsed, setCollapsed] = useState(false);
  const [{ history, loading, error }, setState] = useState<HistoryState>({ history: null, loading: false, error: null });
  const [detail, setDetail] = useState<Detail | null>(null);
  const [focusedOid, setFocusedOid] = useState<string | null>(null);
  const loader = useRef<ReturnType<typeof createGitHistoryLoader> | null>(null);
  const bodyId = useId();
  const cardId = useId();
  const cardRef = useRef<HTMLDivElement | null>(null);
  const originRef = useRef<HTMLElement | null>(null);
  const loadDetails = useMemo(() => createCommitDetailsCache((oid) => gitCommitDetails(folder, oid)), [folder]);
  useEffect(() => {
    if (!detail) return;
    const closeOutside = (event: PointerEvent) => {
      const target = event.target as Node | null;
      if (!cardRef.current?.contains(target) && !originRef.current?.contains(target)) setDetail(null);
    };
    const closeOnEscape = (event: globalThis.KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented) return;
      event.preventDefault();
      if (cardRef.current?.contains(document.activeElement)) originRef.current?.focus({ preventScroll: true });
      setDetail(null);
    };
    document.addEventListener('pointerdown', closeOutside);
    document.addEventListener('keydown', closeOnEscape);
    return () => {
      document.removeEventListener('pointerdown', closeOutside);
      document.removeEventListener('keydown', closeOnEscape);
    };
  }, [detail]);
  useLayoutEffect(() => {
    const card = cardRef.current;
    if (!card || !detail || collapsed) return;
    const position = () => {
      const rect = card.getBoundingClientRect();
      card.style.left = `${Math.max(8, Math.min(detail.left, window.innerWidth - rect.width - 8))}px`;
      card.style.top = `${Math.max(8, Math.min(detail.top, window.innerHeight - rect.height - 8))}px`;
    };
    position();
    const observer = new ResizeObserver(position);
    observer.observe(card);
    window.addEventListener('resize', position);
    return () => { observer.disconnect(); window.removeEventListener('resize', position); };
  }, [detail, collapsed]);
  useEffect(() => {
    const current = createGitHistoryLoader((limit) => gitHistory(folder, limit), setState);
    loader.current = current;
    return () => { current.stop(); loader.current = null; };
  }, [folder]);
  useEffect(() => {
    loader.current?.setExpanded(!collapsed);
  }, [collapsed]);
  const graph = useMemo(() => layoutGitGraph(history?.commits ?? []), [history?.commits]);
  const refsByOid = useMemo(() => {
    const byOid = new Map<string, GitRef[]>();
    for (const ref of history?.refs ?? []) {
      const refs = byOid.get(ref.oid) ?? [];
      refs.push(ref);
      byOid.set(ref.oid, refs);
    }
    for (const refs of byOid.values()) {
      refs.sort((a, b) => Number(b.name === history?.head.refName) - Number(a.name === history?.head.refName)
        || a.kind.localeCompare(b.kind) || a.name.localeCompare(b.name));
    }
    return byOid;
  }, [history]);
  const detailCommit = history?.commits.find((commit) => commit.oid === detail?.oid);
  const focusableOid = history?.commits.some((commit) => commit.oid === focusedOid)
    ? focusedOid : history?.commits[0]?.oid;
  const showDetail = (element: HTMLElement, oid: string) => {
    originRef.current = element;
    const rect = element.getBoundingClientRect();
    setDetail({ oid, left: rect.right + 6, top: rect.top });
  };
  const toggleDetail = (element: HTMLElement, oid: string) => {
    if (detail?.oid === oid) setDetail(null);
    else showDetail(element, oid);
  };
  const onRowKeyDown = (event: KeyboardEvent<HTMLDivElement>, index: number) => {
    if (event.key === 'Escape') { event.preventDefault(); setDetail(null); return; }
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      if (!event.repeat) toggleDetail(event.currentTarget, event.currentTarget.dataset.oid!);
      return;
    }
    if (event.key === 'Tab' && !event.shiftKey && detail?.oid === event.currentTarget.dataset.oid) {
      const action = cardRef.current?.querySelector<HTMLElement>('button:not(:disabled), a[href]');
      if (action) { event.preventDefault(); action.focus(); return; }
    }
    const rows = event.currentTarget.parentElement?.querySelectorAll<HTMLElement>('.git-graph-row');
    if (!rows?.length) return;
    const target = event.key === 'ArrowDown' ? Math.min(index + 1, rows.length - 1)
      : event.key === 'ArrowUp' ? Math.max(index - 1, 0)
        : event.key === 'Home' ? 0 : event.key === 'End' ? rows.length - 1 : null;
    if (target !== null) { event.preventDefault(); rows[target].focus(); }
  };
  return (
    <section className={`git-panel git-graph-panel${collapsed ? ' collapsed' : ''}`} aria-label="Git 提交历史">
      <div className="git-panel-head">
        <button
          className="git-panel-toggle"
          aria-expanded={!collapsed}
          aria-controls={bodyId}
          onClick={() => { setDetail(null); setCollapsed((value) => !value); }}
        >
          <ChevronIcon size={13} className={collapsed ? '' : 'expanded'} />
          <span>Graph</span>
        </button>
        <button
          className="git-refresh"
          title="刷新提交历史"
          aria-label="刷新提交历史"
          disabled={loading || collapsed}
          onClick={() => loader.current?.refresh()}
        >
          {loading ? <span className="git-spinner"><SpinnerIcon size={14} /></span> : <RefreshIcon size={14} />}
        </button>
      </div>
      <div id={bodyId} className="git-panel-body" hidden={collapsed} aria-busy={loading}>
        {error && <div className="git-graph-error" role="alert"><span>{error}</span><button className="git-refresh" disabled={loading} onClick={() => loader.current?.refresh()}>重试</button></div>}
        {!history ? (
          !error && <div className="git-empty" role="status">正在读取提交历史…</div>
        ) : !history.root ? (
          <div className="git-empty">当前项目不在 Git 仓库中</div>
        ) : history.commits.length === 0 ? (
          <div className="git-empty">仓库尚无提交</div>
        ) : (
          <div className="git-graph-scroll" onScroll={(event) => {
            if (!detail) return;
            const origin = originRef.current;
            if (origin?.dataset.oid && event.currentTarget.contains(origin)) {
              const row = origin.getBoundingClientRect();
              const viewport = event.currentTarget.getBoundingClientRect();
              if (row.bottom > viewport.top && row.top < viewport.bottom) {
                showDetail(origin, origin.dataset.oid);
                return;
              }
            }
            setDetail(null);
          }}>
            <div className="git-graph-list" role="list" aria-label="提交历史" style={{ minWidth: graph.width + 140 }}>
              {history.commits.map((commit, index) => {
                const row = graph.rows[index];
                const refs = refsByOid.get(commit.oid) ?? [];
                const isHead = commit.oid === history.head.oid;
                const description = commitDescription(commit, refs, isHead);
                return (
                  <div
                    key={commit.oid}
                    data-oid={commit.oid}
                    role="listitem"
                    className={`git-graph-row${isHead ? ' is-head' : ''}${detail?.oid === commit.oid ? ' is-selected' : ''}`}
                    tabIndex={commit.oid === focusableOid ? 0 : -1}
                    aria-label={description}
                    aria-haspopup="dialog"
                    aria-controls={detail?.oid === commit.oid ? cardId : undefined}
                    onClick={(event) => toggleDetail(event.currentTarget, commit.oid)}
                    onFocus={() => setFocusedOid(commit.oid)}
                    onKeyDown={(event) => onRowKeyDown(event, index)}
                  >
                    <svg className="git-graph-marks" width={graph.width} height={GRAPH_ROW_HEIGHT} aria-hidden="true">
                      {row.segments.map((segment, key) => (
                        <path key={key} className={`git-graph-color-${segment.color}`} d={graphPath(segment)} />
                      ))}
                      <circle className="git-graph-halo" cx={graphX(row.lane)} cy={GRAPH_ROW_HEIGHT / 2} r={6.5} />
                      <circle
                        className={`git-graph-node git-graph-color-${row.color}${isHead ? ' is-head' : ''}`}
                        cx={graphX(row.lane)} cy={GRAPH_ROW_HEIGHT / 2} r={isHead ? 4.5 : 4}
                      />
                    </svg>
                    <span className="git-graph-summary">
                      <span className="git-graph-subject">{commit.subject || '（无标题）'}</span>
                    </span>
                    <span className="git-graph-author">{commit.authorName}</span>
                  </div>
                );
              })}
            </div>
            <div className="git-graph-footer">
              {history.hasMore && history.commits.length < HISTORY_LIMIT ? (
                <button className="git-graph-more" disabled={loading} onClick={() => loader.current?.loadMore()}>{loading ? '正在读取…' : '加载更多'}</button>
              ) : <span>{history.hasMore ? `已显示前 ${HISTORY_LIMIT} 条提交` : `共 ${history.commits.length} 条提交`}</span>}
            </div>
          </div>
        )}
      </div>
      {!collapsed && detail && detailCommit && createPortal(
        <div
          id={cardId} ref={cardRef} role="dialog" aria-label="提交详情"
          className="git-graph-tooltip" style={{ left: detail.left, top: detail.top }}
          onKeyDown={(event) => {
            if (event.key === 'Escape') {
              event.preventDefault();
              originRef.current?.focus({ preventScroll: true });
              setDetail(null);
            } else if (event.key === 'Tab' && event.shiftKey
              && event.target === cardRef.current?.querySelector('button:not(:disabled), a[href]')) {
              event.preventDefault();
              originRef.current?.focus({ preventScroll: true });
            }
          }}
        >
          <GitCommitCard
            key={detailCommit.oid} commit={detailCommit} refs={refsByOid.get(detailCommit.oid) ?? []}
            isHead={detailCommit.oid === history?.head.oid} load={loadDetails}
          />
        </div>, document.body,
      )}
    </section>
  );
}
