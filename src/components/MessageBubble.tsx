import { useState } from 'react';
import { convertFileSrc } from '@tauri-apps/api/core';
import type { Msg } from '../stores/chat';
import { oneLine } from '../format';
import Markdown from './Markdown';
import { ChevronIcon, CodeIcon, SparkleIcon, TerminalIcon } from './Icons';

/**
 * Thumbnails for pasted screenshots. They live in the app cache, which the OS
 * may purge, so a path that no longer resolves is simply hidden.
 */
function BubbleImages({ paths }: { paths: string[] }) {
  const [failed, setFailed] = useState<string[]>([]);
  const visible = paths.filter((p) => !failed.includes(p));
  if (visible.length === 0) return null;
  return (
    <div className="bubble-images">
      {visible.map((path) => (
        <img
          key={path}
          className="bubble-image"
          src={convertFileSrc(path)}
          alt=""
          loading="lazy"
          onError={() => setFailed((prev) => [...prev, path])}
        />
      ))}
    </div>
  );
}

export default function MessageBubble({
  msg,
  onRetry,
  onSendAsIs,
}: {
  msg: Msg;
  onRetry: () => void;
  onSendAsIs: () => void;
}) {
  const isUser = msg.role === 'user';

  // Claude Code stores pasted images as a `[Image #N]` marker inside the
  // prompt text. Restored turns can still carry it, so strip it for display.
  const hasImageMarker = /\[Image #\d+\]/.test(msg.original);
  const userText = msg.original.replace(/\[Image #\d+\]/g, ' ').replace(/\s+/g, ' ').trim();
  // Primary display: user → 中文原文; assistant → 中文译文（无译文时显示英文原文）
  const primary = isUser ? userText : (msg.translation ?? msg.original);
  const secondary = isUser ? msg.translation : msg.original;
  const thinking = msg.thinking?.trim() ?? '';
  // Empty body: an image turn already shows its thumbnails above; a restored
  // image turn (paths are not persisted) says so without the dead thumbnail;
  // only an assistant bubble's emptiness means "still working".
  const emptyBody = msg.images?.length ? null
    : msg.hadImages || hasImageMarker ? <span className="bubble-placeholder">（图片）</span>
      : !isUser && msg.toolCalls.length === 0 && !thinking ? (
        <span className="bubble-placeholder">（思考中…）</span>
      ) : null;

  return (
    <div className={`bubble-row ${isUser ? 'from-user' : 'from-ai'}`}>
      <div className={`bubble ${isUser ? 'user' : 'ai'}`}>
        {thinking && (
          <details className="thinking-line">
            <summary>
              <span className="step-icon"><SparkleIcon size={14} /></span>
              <span className="step-label" title={thinking}>{oneLine(thinking, 96)}</span>
              <ChevronIcon size={13} className="step-chevron" />
            </summary>
            <div className="step-body">
              <div className="thinking-body">{thinking}</div>
            </div>
          </details>
        )}

        {msg.toolCalls.length > 0 && (
          <div className="step-list">
            {msg.toolCalls.map((t) => {
              const StepIcon = t.name === 'Bash' ? TerminalIcon : CodeIcon;
              const label = t.label || t.detail || t.name;
              return (
                <details key={t.id} className={`step-item ${t.isError ? 'is-error' : ''}`}>
                  <summary>
                    <span className="step-icon"><StepIcon size={14} /></span>
                    <span className="step-label" title={t.detail || t.name}>{label}</span>
                    {t.result !== undefined && (
                      <span className="step-status">{t.isError ? '失败' : '✓'}</span>
                    )}
                    <ChevronIcon size={13} className="step-chevron" />
                  </summary>
                  <div className="step-body">
                    {t.detail && <pre className="step-command">{t.detail}</pre>}
                    {t.result !== undefined ? (
                      <pre className="step-output">{t.result || '（无输出）'}</pre>
                    ) : (
                      <div className="step-pending">等待输出…</div>
                    )}
                  </div>
                </details>
              );
            })}
          </div>
        )}

        {msg.images && msg.images.length > 0 && <BubbleImages paths={msg.images} />}

        <div className="bubble-text">
          {primary.trim() ? (isUser ? primary : <Markdown>{primary}</Markdown>) : emptyBody}
        </div>

        {msg.working && <div className="bubble-working">翻译中…</div>}
        {msg.error && (
          <div className="bubble-error">
            <div className="err-text">{msg.error}</div>
            <div className="err-actions">
              <button onClick={onRetry}>重试翻译</button>
              <button onClick={onSendAsIs}>照原文发送</button>
            </div>
          </div>
        )}
        {isUser && secondary && secondary !== msg.original && !msg.error && (
          <div className="bubble-secondary" title="英文译文">{secondary}</div>
        )}
      </div>
    </div>
  );
}
