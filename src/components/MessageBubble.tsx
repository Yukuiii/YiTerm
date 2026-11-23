import { memo, useEffect, useRef, useState } from 'react';
import { convertFileSrc } from '@tauri-apps/api/core';
import type { Msg } from '../stores/chat';
import { formatClock, formatModel, isFreshTurn, stripImageMarkers } from '../format';
import Markdown from './Markdown';
import CopyButton from './CopyButton';
import { LinkedText } from './ContentLink';
import LiveDots from './LiveDots';
import PlanCard, { type PlanControls } from './PlanCard';
import ToolActivity from './ToolActivity';
import { FileIcon } from './Icons';

const COLLAPSE_HEIGHT = 300;

const baseName = (path: string) => path.split(/[\\/]/).pop() || path;

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

export default memo(function MessageBubble({
  msg,
  onRetry,
  onSendAsIs,
  onEditText,
  planControls,
  hideActivity = false,
}: {
  msg: Msg;
  onRetry: (key: string) => void;
  onSendAsIs: (key: string, original: string) => void;
  onEditText?: (text: string) => void;
  planControls?: PlanControls;
  hideActivity?: boolean;
}) {
  const isUser = msg.role === 'user';
  const [expanded, setExpanded] = useState(false);
  const [overflowing, setOverflowing] = useState(false);
  const [enter] = useState(() => isFreshTurn(msg.ts, msg.working));
  const textRef = useRef<HTMLDivElement>(null);

  // Claude Code stores pasted images as a `[Image #N]` marker inside the
  // prompt text. Restored turns can still carry it, so strip it for display.
  const hasImageMarker = /\[Image #\d+\]/.test(msg.original);
  const userText = stripImageMarkers(msg.original);
  // Keep a plan readable while its translation is incomplete or a replay
  // enriches an older cached body. Ordinary replies still stream translated text.
  const isPlan = !isUser && msg.toolCalls.some((t) => t.name === 'ExitPlanMode');
  const translation = isPlan && (
    msg.working || msg.error || !msg.translation?.trim() || msg.translatedFrom !== msg.original
  ) ? null : msg.translation;
  // Primary display: user → 中文原文; assistant → 中文译文（无译文时显示英文原文）
  const primary = isUser ? userText : (translation ?? msg.original);
  const secondary = isUser ? msg.translation : msg.original;
  const thinking = msg.thinking?.trim() ?? '';
  const clock = formatClock(msg.ts);
  const modelLabel = !isUser ? (formatModel(msg.model) ?? 'Claude Code') : '';

  // User prompts can be long; the reference clamps them behind a masked
  // "Show more" instead of letting one message fill the viewport.
  useEffect(() => {
    if (!isUser) return;
    const el = textRef.current;
    if (el) setOverflowing(el.scrollHeight > COLLAPSE_HEIGHT);
  }, [isUser, primary]);

  if (isPlan && primary.trim()) {
    return <PlanCard msg={msg} content={primary} controls={planControls} />;
  }

  // Empty body: an image turn already shows its thumbnails above; a restored
  // image turn (paths are not persisted) says so without the dead thumbnail;
  // only an assistant bubble's emptiness means "still working".
  const emptyBody = msg.images?.length ? null
    : msg.hadImages || hasImageMarker ? <span className="bubble-placeholder">（图片）</span>
      : !isUser && msg.toolCalls.length === 0 && !thinking ? (
        <LiveDots label="思考中" />
      ) : null;

  return (
    <div className={`bubble-row ${isUser ? 'from-user' : 'from-ai'}${enter ? ' chat-enter' : ''}`}>
      <div className={`bubble-stack ${isUser ? 'is-user' : 'is-ai'}`}>
        <div className={`bubble ${isUser ? 'user' : 'ai'}`}>
          {!isUser && primary.trim() && (
            <div className="assistant-head">
              <span className="assistant-name">Claude</span>
              <span className="model-badge">{modelLabel}</span>
              {clock && (
                <>
                  <span className="assistant-sep" aria-hidden="true">·</span>
                  <time className="assistant-time" dateTime={msg.ts ?? undefined}>{clock}</time>
                </>
              )}
            </div>
          )}

          {!hideActivity && (thinking || msg.toolCalls.length > 0) && <ToolActivity messages={[msg]} />}

          {msg.images && msg.images.length > 0 && <BubbleImages paths={msg.images} />}

          {msg.files && msg.files.length > 0 && (
            <div className="bubble-files">
              {msg.files.map((path) => (
                <span className="bubble-file" key={path} title={path}>
                  <FileIcon size={12} />
                  <span className="bubble-file-name">{baseName(path)}</span>
                </span>
              ))}
            </div>
          )}

          <div
            ref={isUser ? textRef : undefined}
            className={`bubble-text ${isUser && overflowing && !expanded ? 'is-clamped' : ''}`}
          >
            {primary.trim() ? (isUser ? <LinkedText>{primary}</LinkedText> : <Markdown>{primary}</Markdown>) : emptyBody}
          </div>

          {isUser && overflowing && (
            <button
              type="button"
              className="bubble-expand"
              aria-expanded={expanded}
              onClick={() => setExpanded((value) => !value)}
            >
              {expanded ? '收起' : '展开'}
            </button>
          )}

          {msg.working && <div className="bubble-working">翻译中…</div>}
          {msg.error && (
            <div className="bubble-error">
              <div className="err-text">{msg.error}</div>
              <div className="err-actions">
                <button onClick={() => onRetry(msg.key)}>重试翻译</button>
                <button onClick={() => onSendAsIs(msg.key, msg.original)}>照原文发送</button>
              </div>
            </div>
          )}
          {isUser && secondary && secondary !== msg.original && !msg.error && (
            <div className="bubble-secondary" title="英文译文"><LinkedText>{secondary}</LinkedText></div>
          )}
          {isUser && msg.interrupted && (
            <div className="bubble-interrupted-row">
              <span className="bubble-interrupted" title="发送后未收到回复即被中断">
                已中断
              </span>
            </div>
          )}
        </div>

        {(primary.trim() || (isUser && clock)) && (
          <div className="bubble-meta">
            {isUser && clock && (
              <time className="message-time" dateTime={msg.ts ?? undefined}>{clock}</time>
            )}
            {primary.trim() && (
              <div className="message-actions" role="toolbar" aria-label="消息操作">
                <CopyButton text={primary} className="bubble-action" />
                {!isUser && secondary && secondary !== primary && (
                  <CopyButton text={msg.original} className="bubble-action" label="复制原文" />
                )}
                {isUser && onEditText && (
                  <button
                    type="button"
                    className="bubble-action"
                    onClick={() => onEditText(primary)}
                  >
                    编辑
                  </button>
                )}
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
});
