import { useEffect, useRef, useState } from 'react';
import { convertFileSrc } from '@tauri-apps/api/core';
import { revealItemInDir } from '@tauri-apps/plugin-opener';
import { chatImageSave, encodeBytesToBase64, hasCJK, terminalSendPrompt } from '../ipc';
import { useChatStore } from '../stores/chat';
import { useConversationsStore } from '../stores/conversations';
import { useTerminalsStore } from '../stores/terminals';
import MessageBubble from './MessageBubble';
import { ArrowDownIcon, ArrowUpIcon, FolderIcon, XIcon } from './Icons';

interface PendingImage {
  id: string;
  path: string;
}

export default function ChatPanel({ convId }: { convId: string }) {
  const msgs = useChatStore((s) => s.byConv[convId]) ?? [];
  const conv = useConversationsStore((s) => s.list.find((c) => c.id === convId));
  const terminal = useTerminalsStore((s) => s.byConv[convId]);
  const exited = useTerminalsStore((s) => (convId in s.exited ? s.exited[convId] : undefined));
  const pending = useChatStore((s) => s.pendingPrompts[convId]);
  const [input, setInput] = useState('');
  const [sending, setSending] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [images, setImages] = useState<PendingImage[]>([]);
  const [savingImages, setSavingImages] = useState(0);
  const messagesRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  // Whether the list is parked at the newest message. While it isn't (the
  // user scrolled up to read), streaming updates must not yank the view down;
  // a floating button offers the jump instead.
  const atBottomRef = useRef(true);
  const [atBottom, setAtBottom] = useState(true);

  const scrollToBottom = (behavior: ScrollBehavior = 'auto') => {
    const el = messagesRef.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior });
    atBottomRef.current = true;
    setAtBottom(true);
  };

  const onMessagesScroll = () => {
    const el = messagesRef.current;
    if (!el) return;
    const bottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    atBottomRef.current = bottom;
    setAtBottom(bottom);
  };

  // One row by default; grow with content up to the CSS max-height so the
  // composer never reserves an empty second line (browser default is rows=2).
  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
  }, [input]);

  const flashNotice = (text: string) => {
    setNotice(text);
    window.setTimeout(() => setNotice(null), 3000);
  };

  useEffect(() => {
    void useChatStore.getState().hydrate(convId);
  }, [convId]);

  const lastMsg = msgs[msgs.length - 1];
  useEffect(() => {
    // Scroll only the message list. `scrollIntoView` also scrolls the document
    // (including horizontally), which could shift the sidebar out of view.
    // Skip while the user is reading history further up.
    if (atBottomRef.current) scrollToBottom('smooth');
  }, [msgs.length, lastMsg?.translation, lastMsg?.original]);

  // Conversation switch: start at the newest message.
  useEffect(() => {
    atBottomRef.current = true;
    scrollToBottom();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [convId]);

  /** Persists clipboard images and adds a removable chip for each. */
  const addPastedImages = async (files: File[]) => {
    setSavingImages((n) => n + files.length);
    for (const file of files) {
      try {
        const bytes = new Uint8Array(await file.arrayBuffer());
        const path = await chatImageSave(convId, file.type, encodeBytesToBase64(bytes));
        setImages((prev) => [...prev, { id: crypto.randomUUID(), path }]);
      } catch (e) {
        flashNotice(`图片添加失败：${String(e)}`);
      } finally {
        setSavingImages((n) => n - 1);
      }
    }
  };

  const onPaste = (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
    // WebKit exposes pasteboard images through `files` (it converts TIFF to
    // PNG); `items` is the fallback. An empty type still goes to the backend,
    // which sniffs the real content.
    const files: File[] = Array.from(e.clipboardData.files).filter(
      (f) => f.type === '' || f.type.startsWith('image/'),
    );
    if (files.length === 0) {
      for (const item of Array.from(e.clipboardData.items)) {
        if (item.kind === 'file' && item.type.startsWith('image/')) {
          const f = item.getAsFile();
          if (f) files.push(f);
        }
      }
    }
    if (files.length === 0) return; // plain-text paste: leave it to the textarea
    e.preventDefault();
    void addPastedImages(files);
  };

  const send = async (override?: string) => {
    const text = (override ?? input).trim();
    // Only the composer's own images belong to this turn; `override` comes from
    // the landing-screen prompt queue, which is text-only.
    const paths = override === undefined ? images.map((i) => i.path) : [];
    if ((!text && paths.length === 0) || sending || savingImages > 0) return;
    const termId = terminal?.terminalId;
    if (exited !== undefined) {
      flashNotice('终端已退出，无法发送');
      return;
    }
    if (termId === undefined) {
      flashNotice('终端正在启动，请稍候…');
      return;
    }
    // Clear before recording: from here the bubble owns the images, and a
    // translation failure is recoverable through 重试翻译 / 照原文发送.
    if (override === undefined) {
      setInput('');
      setImages([]);
    }
    // A sent turn always brings the view to the newest message.
    scrollToBottom();
    setSending(true);
    const store = useChatStore.getState();
    try {
      if (hasCJK(text)) {
        const key = store.addOptimistic(convId, text, paths);
        const en = await store.translateOutbound(convId, key, text);
        // Record before writing: the JSONL line can land within milliseconds
        // of the write and reconcile relies on this text.
        useChatStore.getState().markSent(convId, key, en);
        await terminalSendPrompt(termId, en, paths);
      } else {
        store.addPlain(convId, text, paths);
        await terminalSendPrompt(termId, text, paths);
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

  // Prompt submitted from the landing screen: wait until the PTY exists.
  useEffect(() => {
    if (!pending || exited !== undefined) return;
    if (terminal?.terminalId === undefined) return;
    const text = useChatStore.getState().takePrompt(convId);
    if (text) void send(text);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pending, terminal?.terminalId, exited, convId]);

  const sendAsIs = async (key: string, original: string) => {
    const termId = terminal?.terminalId;
    if (termId === undefined) return;
    const m = (useChatStore.getState().byConv[convId] ?? []).find((x) => x.key === key);
    useChatStore.getState().setWorking(convId, key, false);
    // A failed translation must not silently drop the screenshots.
    await terminalSendPrompt(termId, original, m?.images ?? []);
  };

  const retryTranslate = async (key: string) => {
    const m = (useChatStore.getState().byConv[convId] ?? []).find((x) => x.key === key);
    if (!m) return;
    useChatStore.getState().setWorking(convId, key, true);
    try {
      const en = await useChatStore.getState().translateOutbound(convId, key, m.original);
      const termId = terminal?.terminalId;
      if (termId !== undefined) await terminalSendPrompt(termId, en, m.images ?? []);
    } catch (e) {
      useChatStore.getState().setError(convId, key, String(e));
    }
  };

  return (
    <div className="chat-panel">
      <div className="messages" ref={messagesRef} onScroll={onMessagesScroll}>
        {msgs.length === 0 && (
          <div className="msg-empty">
            用中文输入需求，会自动翻译成英文发送给 Claude Code；
            <br />
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
      </div>

      {notice && <div className="composer-notice">{notice}</div>}

      <div className="composer-wrap">
        {!atBottom && (
          <button
            className="scroll-bottom-btn"
            title="回到最新"
            onClick={() => scrollToBottom()}
          >
            <ArrowDownIcon size={15} />
          </button>
        )}
        {images.length > 0 && (
          <div className="composer-attachments">
            {images.map((img) => (
              <div className="attachment-chip" key={img.id}>
                <img src={convertFileSrc(img.path)} alt="" />
                <button
                  className="attachment-remove"
                  title="移除图片"
                  onClick={() => setImages((prev) => prev.filter((x) => x.id !== img.id))}
                >
                  <XIcon size={11} />
                </button>
              </div>
            ))}
          </div>
        )}
        <div className="composer">
          <button
            className="composer-icon-btn"
            title="在访达中显示项目文件夹"
            onClick={() => {
              if (conv) void revealItemInDir(conv.folder).catch(() => {});
            }}
          >
            <FolderIcon size={17} />
          </button>
          <textarea
            ref={inputRef}
            value={input}
            rows={1}
            placeholder={exited !== undefined ? '终端已退出' : '输入中文（Enter 发送，Shift+Enter 换行）'}
            disabled={exited !== undefined}
            onChange={(e) => setInput(e.target.value)}
            onPaste={onPaste}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault();
                void send();
              }
            }}
          />
          <button
            className="send-btn"
            title="发送"
            onClick={() => void send()}
            disabled={sending || savingImages > 0 || (!input.trim() && images.length === 0)}
          >
            <ArrowUpIcon size={16} />
          </button>
        </div>
      </div>
    </div>
  );
}
