import { useEffect, useState } from 'react';
import type { GitCommit, GitCommitDetails, GitRef } from '../ipc';
import { formatCommitAge, formatCommitDate } from '../gitCommitDetails';
import ContentLink from './ContentLink';
import { CheckIcon, CloudIcon, CopyIcon, GitBranchIcon, GlobeIcon, TagIcon } from './Icons';

type CopyState = 'idle' | 'copied' | 'failed';

interface ContentProps {
  commit: GitCommit;
  refs: GitRef[];
  isHead: boolean;
  details: GitCommitDetails | null;
  error: string | null;
  copyState: CopyState;
  onCopy: () => void;
  onRetry: () => void;
}

export function GitCommitCardContent({ commit, refs, isHead, details, error, copyState, onCopy, onRetry }: ContentProps) {
  const initials = Array.from(commit.authorName.trim() || '?').slice(0, 1).join('').toUpperCase();
  return (
    <>
      <div className="git-commit-header">
        <span className="git-commit-avatar" aria-hidden="true">{initials}</span>
        <span className="git-commit-author">{commit.authorName || '未知作者'}</span>
        <span className="git-commit-date">{formatCommitAge(commit.authorTime)}（{formatCommitDate(commit.authorTime)}）</span>
      </div>
      <div className="git-commit-message">{details?.message || commit.subject || '（无标题）'}</div>
      <div className="git-commit-stats" title={commit.parents.length > 1 ? '与第一父提交相比的改动' : undefined}>
        {error ? (
          <><span role="alert">读取提交详情失败</span><button className="git-commit-action" title={error} onClick={onRetry}>重试</button></>
        ) : details ? (
          <span>{details.filesChanged} 个文件改动，<span className="insertions">{details.insertions} 行新增(+)</span>，<span className="deletions">{details.deletions} 行删除(-)</span></span>
        ) : <span role="status">正在读取改动统计…</span>}
      </div>
      {(isHead || refs.length > 0) && (
        <div className="git-commit-refs">
          {isHead && <span className="git-commit-ref is-head">HEAD</span>}
          {refs.map((ref) => {
            const Icon = ref.kind === 'tag' ? TagIcon : ref.kind === 'remote' ? CloudIcon : GitBranchIcon;
            return <span className={`git-commit-ref is-${ref.kind}`} key={ref.name} title={ref.name}><Icon size={11} /><span>{ref.shortName}</span></span>;
          })}
        </div>
      )}
      <div className="git-commit-actions">
        <button className="git-commit-action git-commit-copy" onClick={onCopy} title={commit.oid} aria-label="复制完整提交 SHA">
          <span aria-hidden="true">↟</span><code>{commit.oid.slice(0, 7)}</code>{copyState === 'copied' ? <CheckIcon size={12} /> : <CopyIcon size={12} />}
        </button>
        {details?.githubUrl && <ContentLink href={details.githubUrl}><GlobeIcon size={12} />在 GitHub 中打开</ContentLink>}
        {copyState === 'copied' && <span className="git-commit-copy-status" role="status">已复制</span>}
        {copyState === 'failed' && <span className="git-commit-copy-status" role="alert">复制失败，请重试</span>}
      </div>
    </>
  );
}

export default function GitCommitCard({ commit, refs, isHead, load }: {
  commit: GitCommit;
  refs: GitRef[];
  isHead: boolean;
  load: (oid: string) => Promise<GitCommitDetails>;
}) {
  const [details, setDetails] = useState<GitCommitDetails | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [copyState, setCopyState] = useState<CopyState>('idle');
  useEffect(() => {
    let disposed = false;
    void load(commit.oid).then((value) => {
      if (!disposed) setDetails(value);
    }).catch((error: unknown) => {
      if (!disposed) setError(String(error));
    });
    return () => { disposed = true; };
  }, [commit.oid, load, attempt]);
  useEffect(() => {
    if (copyState === 'idle') return;
    const timer = window.setTimeout(() => setCopyState('idle'), 2000);
    return () => window.clearTimeout(timer);
  }, [copyState]);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(commit.oid);
      setCopyState('copied');
    } catch {
      setCopyState('failed');
    }
  };
  return <GitCommitCardContent
    commit={commit} refs={refs} isHead={isHead} details={details} error={error} copyState={copyState}
    onCopy={() => void copy()} onRetry={() => { setError(null); setAttempt((value) => value + 1); }}
  />;
}
