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
        {msg.toolCalls.length > 0 && (
          <div className="tool-list">
            {msg.toolCalls.map((t) => (
              <details key={t.id} className={`tool-item ${t.isError ? 'is-error' : ''}`}>
                <summary>
                  <span className="chip">{t.name}</span>
                  <span className="tool-detail" title={t.detail}>{t.detail || '—'}</span>
                  {t.result !== undefined && (
                    <span className="tool-status">{t.isError ? '失败' : '✓'}</span>
                  )}
                </summary>
                {t.result !== undefined ? (
                  <pre className="tool-output">{t.result || '（无输出）'}</pre>
                ) : (
                  <div className="tool-pending">等待输出…</div>
                )}
              </details>
            ))}
          </div>
        )}
        <div className="bubble-text">
          {primary.trim() ? (
            primary
          ) : msg.toolCalls.length === 0 ? (
            <span className="bubble-placeholder">（思考中…）</span>
          ) : null}
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
