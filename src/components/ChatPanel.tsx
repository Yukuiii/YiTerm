import { useEffect, useRef, useState } from 'react';
import { hasCJK, terminalWrite } from '../ipc';
import { useChatStore } from '../stores/chat';
import { useConversationsStore } from '../stores/conversations';
import { useTerminalsStore } from '../stores/terminals';
import MessageBubble from './MessageBubble';

export default function ChatPanel({ convId }: { convId: string }) {
  const msgs = useChatStore((s) => s.byConv[convId]) ?? [];
  const conv = useConversationsStore((s) => s.list.find((c) => c.id === convId));
  const terminal = useTerminalsStore((s) => s.byConv[convId]);
  const exited = useTerminalsStore((s) => (convId in s.exited ? s.exited[convId] : undefined));
  const [input, setInput] = useState('');
  const [sending, setSending] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const bottomRef = useRef<HTMLDivElement>(null);

  const flashNotice = (text: string) => {
    setNotice(text);
    window.setTimeout(() => setNotice(null), 3000);
  };

  useEffect(() => {
    void useChatStore.getState().hydrate(convId);
  }, [convId]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [msgs.length, msgs[msgs.length - 1]?.translation]);

  const send = async () => {
    const text = input.trim();
    if (!text || sending) return;
    const termId = terminal?.terminalId;
    if (exited !== undefined) {
      flashNotice('终端已退出，无法发送');
      return;
    }
    if (termId === undefined) {
      flashNotice('终端正在启动，请稍候…');
      return;
    }
    setInput('');
    setSending(true);
    const store = useChatStore.getState();
    try {
      if (hasCJK(text)) {
        const key = store.addOptimistic(convId, text);
        const en = await store.translateOutbound(convId, key, text);
        // Record before writing: the JSONL line can land within milliseconds
        // of the write and reconcile relies on this text.
        useChatStore.getState().markSent(convId, key, en);
        await terminalWrite(termId, en, true);
      } else {
        store.addPlain(convId, text);
        await terminalWrite(termId, text, true);
      }
    } catch (e) {
      // 翻译失败：原文已在气泡中，给出发送选项
      const list = useChatStore.getState().byConv[convId] ?? [];
      const last = list[list.length - 1];
      if (last && last.role === 'user' && last.working) {
        useChatStore.getState().setError(convId, last.key, String(e));
      }
    } finally {
      setSending(false);
    }
  };

  const sendAsIs = async (key: string, original: string) => {
    const termId = terminal?.terminalId;
    if (termId === undefined) return;
    useChatStore.getState().setWorking(convId, key, false);
    await terminalWrite(termId, original, true);
  };

  const retryTranslate = async (key: string) => {
    const m = (useChatStore.getState().byConv[convId] ?? []).find((x) => x.key === key);
    if (!m) return;
    useChatStore.getState().setWorking(convId, key, true);
    try {
      const en = await useChatStore.getState().translateOutbound(convId, key, m.original);
      const termId = terminal?.terminalId;
      if (termId !== undefined) await terminalWrite(termId, en, true);
    } catch (e) {
      useChatStore.getState().setError(convId, key, String(e));
    }
  };

  return (
    <div className="chat-panel">
      <div className="panel-head">
        <span className="panel-title">{conv?.title ?? '会话'}</span>
        {exited !== undefined && (
          <span className="term-status exited">终端已退出{exited !== null ? ` (${exited})` : ''}</span>
        )}
      </div>
      <div className="messages">
        {msgs.length === 0 && (
          <div className="msg-empty">
            用中文输入需求，会自动翻译成英文发送给 Claude Code；<br />
            Claude 的英文回复会自动翻译成中文显示。
          </div>
        )}
        {msgs.map((m) => (
          <MessageBubble
            key={m.key}
            msg={m}
            onRetry={() => void retryTranslate(m.key)}
            onSendAsIs={() => void sendAsIs(m.key, m.original)}
          />
        ))}
        <div ref={bottomRef} />
      </div>
      {notice && <div className="composer-notice">{notice}</div>}
      <div className="composer">
        <textarea
          value={input}
          placeholder={exited !== undefined ? '终端已退出' : '输入中文（Enter 发送，Shift+Enter 换行）'}
          disabled={exited !== undefined}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              void send();
            }
          }}
        />
        <button className="send-btn" onClick={() => void send()} disabled={sending || !input.trim()}>
          发送
        </button>
      </div>
    </div>
  );
}
