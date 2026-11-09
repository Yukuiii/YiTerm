import { memo, useState } from 'react';
import { convertFileSrc } from '@tauri-apps/api/core';
import type { Msg } from '../stores/chat';
import Markdown from './Markdown';
import PlanCard, { type PlanControls } from './PlanCard';
import ToolActivity from './ToolActivity';
import { FileIcon } from './Icons';

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
  planControls,
  hideActivity = false,
}: {
  msg: Msg;
  onRetry: (key: string) => void;
  onSendAsIs: (key: string, original: string) => void;
  planControls?: PlanControls;
  hideActivity?: boolean;
}) {
  const isUser = msg.role === 'user';

  // Claude Code stores pasted images as a `[Image #N]` marker inside the
  // prompt text. Restored turns can still carry it, so strip it for display.
  const hasImageMarker = /\[Image #\d+\]/.test(msg.original);
  const userText = msg.original.replace(/\[Image #\d+\]/g, ' ').replace(/\s+/g, ' ').trim();
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

  if (isPlan && primary.trim()) {
    return <PlanCard msg={msg} content={primary} controls={planControls} />;
  }

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

        <div className="bubble-text">
          {primary.trim() ? (isUser ? primary : <Markdown>{primary}</Markdown>) : emptyBody}
        </div>

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
          <div className="bubble-secondary" title="英文译文">{secondary}</div>
        )}
        {isUser && msg.interrupted && (
          <div className="bubble-interrupted-row">
            <span className="bubble-interrupted" title="发送后未收到回复即被中断">
              已中断
            </span>
          </div>
        )}
      </div>
    </div>
  );
});
