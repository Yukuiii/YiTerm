import { useState } from 'react';
import type { Msg } from '../stores/chat';

export default function MessageBubble({
  msg,
  onRetry,
  onSendAsIs,
}: {
  msg: Msg;
  onRetry: () => void;
  onSendAsIs: () => void;
}) {
  const [showOriginal, setShowOriginal] = useState(false);
  const isUser = msg.role === 'user';

  // Primary display: user → 中文原文; assistant → 中文译文（无译文时显示英文原文）
  const primary = isUser ? msg.original : (msg.translation ?? msg.original);
  const secondary = isUser ? msg.translation : msg.original;

  return (
    <div className={`bubble-row ${isUser ? 'from-user' : 'from-ai'}`}>
      <div className={`bubble ${isUser ? 'user' : 'ai'}`}>
        {msg.tools.length > 0 && (
          <div className="tool-chips">
            {dedupeTools(msg.tools).map((t) => (
              <span key={t} className="chip">{t}</span>
            ))}
          </div>
        )}
        <div className="bubble-text">{primary}</div>
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
        {isUser && secondary && !msg.error && (
          <div className="bubble-secondary" title="英文译文">{secondary}</div>
        )}
        {!isUser && !msg.error && (
          <button className="toggle-original" onClick={() => setShowOriginal(!showOriginal)}>
            {showOriginal ? '隐藏原文' : '显示原文'}
          </button>
        )}
        {!isUser && showOriginal && (
          <div className="bubble-secondary">{msg.original}</div>
        )}
        {msg.thinking && (
          <details className="thinking">
            <summary>思考过程</summary>
            <pre>{msg.thinking}</pre>
          </details>
        )}
      </div>
    </div>
  );
}

function dedupeTools(tools: string[]): string[] {
  const counts = new Map<string, number>();
  for (const t of tools) counts.set(t, (counts.get(t) ?? 0) + 1);
  return [...counts.entries()].map(([t, n]) => (n > 1 ? `${t} ×${n}` : t));
}
