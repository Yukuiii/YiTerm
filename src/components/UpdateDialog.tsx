import { useState } from 'react';
import {
  updateDownload,
  updateInstall,
  type UpdateInfo,
  type UpdateProgress,
} from '../ipc';
import Markdown from './Markdown';
import { XIcon } from './Icons';

type Phase = 'idle' | 'downloading' | 'installing' | 'error';

const fmtMB = (bytes: number) => (bytes / (1024 * 1024)).toFixed(1);

export default function UpdateDialog({
  info,
  onClose,
}: {
  info: UpdateInfo;
  onClose: () => void;
}) {
  const [phase, setPhase] = useState<Phase>('idle');
  const [downloaded, setDownloaded] = useState(0);
  const [total, setTotal] = useState(0);
  const [error, setError] = useState<string | null>(null);

  const busy = phase === 'downloading' || phase === 'installing';
  const pct = total > 0 ? Math.min(100, Math.floor((downloaded / total) * 100)) : 0;

  const start = async () => {
    setPhase('downloading');
    setError(null);
    setDownloaded(0);
    setTotal(0);
    try {
      await updateDownload(info.assetUrl, (p: UpdateProgress) => {
        if (p.type === 'progress') {
          setDownloaded(p.downloaded);
          setTotal(p.total);
        }
      });
      setPhase('installing');
      await updateInstall();
    } catch (e) {
      setError(String(e));
      setPhase('error');
    }
  };

  return (
    <div className="settings-overlay">
      <div className="update-card">
        <div className="update-head">
          <div>
            <h2>发现新版本 {info.latestVersion}</h2>
            <div className="update-sub">
              当前版本 {info.currentVersion}
              {info.assetName ? ` · ${info.assetName}` : ''}
            </div>
          </div>
          <button className="icon-btn" onClick={onClose} disabled={busy} aria-label="关闭">
            <XIcon size={16} />
          </button>
        </div>

        <div className="update-notes">
          {info.notes.trim() ? <Markdown>{info.notes}</Markdown> : <p>新版本已发布。</p>}
        </div>

        {phase === 'downloading' && (
          <div className="update-progress-wrap">
            <div className="update-progress">
              <div
                className={`update-progress-fill ${total > 0 ? '' : 'indeterminate'}`}
                style={{ width: total > 0 ? `${pct}%` : '35%' }}
              />
            </div>
            <div className="update-progress-text">
              {total > 0
                ? `正在下载 ${pct}%（${fmtMB(downloaded)} / ${fmtMB(total)} MB）`
                : `正在下载 ${fmtMB(downloaded)} MB`}
            </div>
          </div>
        )}
        {phase === 'installing' && (
          <div className="update-progress-text">正在安装并重启…</div>
        )}
        {error && <div className="update-error">{error}</div>}

        <div className="update-actions">
          <button className="ghost-btn" onClick={onClose} disabled={busy}>
            稍后
          </button>
          <button className="primary-btn" onClick={() => void start()} disabled={busy}>
            {phase === 'error' ? '重试下载并安装' : '下载并安装'}
          </button>
        </div>
      </div>
    </div>
  );
}
