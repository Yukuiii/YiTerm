import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { convertFileSrc } from '@tauri-apps/api/core';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import {
  chatAttachmentImport,
  chatImageSave,
  encodeBytesToBase64,
  hasCJK,
  terminalSendPrompt,
  terminalWrite,
} from '../ipc';
import { useChatStore, type Msg } from '../stores/chat';
import { useTerminalsStore } from '../stores/terminals';
import { usePlanApproval } from '../hooks/usePlanApproval';
import { useComposerKeyboard } from '../hooks/useComposerKeyboard';
import { latestPlanCall } from '../planApproval';
import { groupToolActivity } from '../toolActivity';
import MessageBubble from './MessageBubble';
import ToolActivity from './ToolActivity';
import { ArrowDownIcon, ArrowUpIcon, FileIcon, PaperclipIcon, XIcon } from './Icons';

const EMPTY_MESSAGES: Msg[] = [];

interface PendingImage {
  id: string;
  path: string;
}

interface PendingFile {
  id: string;
  path: string;
  name: string;
}

/**
 * Appends file references to the already-translated prompt. Paths are not
 * translated; Claude reads the files by path. Kept on the same line: a
 * newline in the burst makes Claude Code treat it as a paste and swallow
 * the Enter that follows.
 */
function withFiles(text: string, files: string[]): string {
  if (files.length === 0) return text;
  const refs = files.map((p) => `\`${p}\``).join(' ');
  return text.trim() ? `${text.trimEnd()} ${refs}` : refs;
}

export default function ChatPanel({ convId, onOpenTerminal }: { convId: string; onOpenTerminal: () => void }) {
  const msgs = useChatStore((s) => s.byConv[convId]) ?? EMPTY_MESSAGES;
  const rows = useMemo(() => groupToolActivity(msgs), [msgs]);
  const terminal = useTerminalsStore((s) => s.byConv[convId]);
  const exited = useTerminalsStore((s) => (convId in s.exited ? s.exited[convId] : undefined));
  const latestPlan = latestPlanCall(msgs);
  const pendingPlan = latestPlan && !latestPlan.resolved ? latestPlan : null;
  const { state: planState, decide: decidePlan } = usePlanApproval(
    convId, exited === undefined ? terminal?.terminalId : undefined, pendingPlan?.callId,
  );
  const planBlocked = !!planState.status?.available || planState.submitting || planState.sent;
  const planControls = pendingPlan ? { state: planState, onDecision: decidePlan, onOpenTerminal } : undefined;
  const pending = useChatStore((s) => s.pendingPrompts[convId]);
  const [input, setInput] = useState('');
  const [sending, setSending] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [images, setImages] = useState<PendingImage[]>([]);
  const [files, setFiles] = useState<PendingFile[]>([]);
  const [attaching, setAttaching] = useState(0);
  const messagesRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  // One send at a time; a newer token invalidates an in-flight translation
  // when Esc cancels the prompt before it reaches the terminal.
  const sendTokenRef = useRef(0);
  const sendingRef = useRef(false);
  const sentRef = useRef(false);
  const sendAbortRef = useRef<AbortController | null>(null);
  // Whether the list is parked at the newest message. While it isn't (the
  // user scrolled up to read), streaming updates must not yank the view down;
  // a floating button offers the jump instead.
  const atBottomRef = useRef(true);
  const [atBottom, setAtBottom] = useState(true);
  // The first content after a conversation switch is scrolled instantly; a
  // smooth animation racing the swapped content can leave the pane unpainted
  // in WKWebView until the next wheel scroll.
  const justSwitchedRef = useRef(false);

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

  // Conversation switch: start at the newest message. Declared before the
  // update effect so the first scroll never starts a smooth animation that
  // races the swapped content.
  useEffect(() => {
    justSwitchedRef.current = true;
    // `scrollToBottom` also parks the list at the bottom.
    scrollToBottom();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [convId]);

  const lastMsg = msgs[msgs.length - 1];
  useEffect(() => {
    // Scroll only the message list. `scrollIntoView` also scrolls the document
    // (including horizontally), which could shift the sidebar out of view.
    // Skip while the user is reading history further up.
    if (!atBottomRef.current) return;
    // After a switch, wait for the conversation's first messages
    // (hydration/replay) so the whole initial jump stays instant.
    if (justSwitchedRef.current && msgs.length === 0) return;
    scrollToBottom(justSwitchedRef.current ? 'auto' : 'smooth');
    justSwitchedRef.current = false;
  }, [msgs.length, lastMsg?.translation, lastMsg?.original, planState.status?.available,
    planState.submitting, planState.sent, planState.error, notice]);

  /** Persists clipboard images and adds a removable chip for each. */
  const addPastedImages = async (picked: File[]) => {
    setAttaching((n) => n + picked.length);
    for (const file of picked) {
      try {
        const bytes = new Uint8Array(await file.arrayBuffer());
        const path = await chatImageSave(convId, file.type, encodeBytesToBase64(bytes));
        setImages((prev) => [...prev, { id: crypto.randomUUID(), path }]);
      } catch (e) {
        flashNotice(`图片添加失败：${String(e)}`);
      } finally {
        setAttaching((n) => n - 1);
      }
    }
  };

  /** Files chosen from disk: images join the screenshot flow, other files are
   *  referenced by path and read by Claude. */
  const pickAttachments = async () => {
    let picked: string | string[] | null = null;
    try {
      picked = await openDialog({ multiple: true, title: '选择附件' });
    } catch (e) {
      flashNotice(`选择附件失败：${String(e)}`);
      return;
    }
    const paths = picked === null ? [] : Array.isArray(picked) ? picked : [picked];
    if (paths.length === 0) return;
    setAttaching((n) => n + paths.length);
    for (const path of paths) {
      try {
        const att = await chatAttachmentImport(convId, path);
        if (att.kind === 'image') {
          setImages((prev) => [...prev, { id: crypto.randomUUID(), path: att.path }]);
        } else {
          setFiles((prev) => [
            ...prev,
            { id: crypto.randomUUID(), path: att.path, name: att.name },
          ]);
        }
      } catch (e) {
        flashNotice(`附件添加失败：${String(e)}`);
      } finally {
        setAttaching((n) => n - 1);
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
    if (planBlocked) {
      flashNotice('请先选择计划执行方式，或在终端提出修改意见。');
      return;
    }
    const text = (override ?? input).trim();
    // Only the composer's own attachments belong to this turn; `override`
    // comes from the landing-screen prompt queue, which is text-only.
    const paths = override === undefined ? images.map((i) => i.path) : [];
    const filePaths = override === undefined ? files.map((f) => f.path) : [];
    if ((!text && paths.length === 0 && filePaths.length === 0) || sending || attaching > 0) return;
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
      setFiles([]);
    }
    // A sent turn always brings the view to the newest message.
    scrollToBottom();
    setSending(true);
    sendingRef.current = true;
    sentRef.current = false;
    const token = ++sendTokenRef.current;
    const store = useChatStore.getState();
    let controller: AbortController | undefined;
    let messageKey: string | undefined;
    try {
      if (hasCJK(text)) {
        const key = store.addOptimistic(convId, text, paths, filePaths);
        messageKey = key;
        controller = new AbortController();
        sendAbortRef.current = controller;
        const en = await store.translateOutbound(convId, key, text, controller.signal);
        if (token !== sendTokenRef.current) {
          // Esc canceled the send while it was being translated; the prompt
          // must not reach the terminal.
          useChatStore.getState().setWorking(convId, key, false);
          useChatStore.getState().markMessageInterrupted(convId, key);
          return;
        }
        const sent = withFiles(en, filePaths);
        // Record before writing: the JSONL line can land within milliseconds
        // of the write and reconcile relies on this text.
        useChatStore.getState().markSent(convId, key, sent);
        sentRef.current = true;
        await terminalSendPrompt(termId, sent, paths);
      } else {
        const sent = withFiles(text, filePaths);
        const key = store.addPlain(convId, text, paths, filePaths);
        messageKey = key;
        // The bubble shows the typed text; reconcile against what Claude saw.
        if (filePaths.length) store.markSent(convId, key, sent);
        sentRef.current = true;
        await terminalSendPrompt(termId, sent, paths);
      }
    } catch (e) {
      if (token !== sendTokenRef.current) return; // canceled; ignore the failure
      // Keep translation and rejected PTY submissions visible and retryable.
      if (messageKey) {
        useChatStore.getState().setError(convId, messageKey, String(e));
      }
    } finally {
      if (controller && sendAbortRef.current === controller) {
        sendAbortRef.current = null;
      }
      // A canceled send may have been superseded by a newer one; don't clear
      // the newer send's state.
      if (token === sendTokenRef.current) {
        sendingRef.current = false;
        setSending(false);
      }
    }
  };

  const composerKeyboard = useComposerKeyboard(() => send());

  // Prompt submitted from the landing screen: wait until the PTY exists.
  useEffect(() => {
    if (!pending || exited !== undefined || planBlocked) return;
    if (terminal?.terminalId === undefined) return;
    const text = useChatStore.getState().takePrompt(convId);
    if (text) void send(text);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pending, terminal?.terminalId, exited, convId, planBlocked]);

  // Esc on the chat page interrupts the running turn, mirroring the terminal.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || e.defaultPrevented || e.isComposing) return;
      const target = e.target instanceof Element ? e.target : null;
      // Menus/modals/sidebar own their Esc.
      if (target?.closest('.sidebar, .settings-overlay, .dropdown-menu')) return;
      // A prompt still in its outbound translation has not reached Claude yet;
      // Esc cancels the send instead of interrupting an idle terminal.
      if (sendingRef.current && !sentRef.current) {
        e.preventDefault();
        sendTokenRef.current++;
        sendAbortRef.current?.abort();
        sendAbortRef.current = null;
        sendingRef.current = false;
        setSending(false);
        const store = useChatStore.getState();
        const cur = store.byConv[convId] ?? [];
        const last = cur[cur.length - 1];
        if (last?.role === 'user') {
          store.setWorking(convId, last.key, false);
          store.markMessageInterrupted(convId, last.key);
        }
        return;
      }
      // xterm already writes Esc to the PTY.
      if (target?.closest('.xterm')) return;
      const list = useChatStore.getState().byConv[convId] ?? [];
      const pending = list.length > 0 && list[list.length - 1].role === 'user';
      // The busy flag comes from a 300 ms transcript poll; a trailing user
      // bubble also means a turn may be waiting for its first reply.
      if (!useTerminalsStore.getState().busyConv[convId] && !pending) return;
      const termId = useTerminalsStore.getState().byConv[convId]?.terminalId;
      if (termId === undefined) return;
      e.preventDefault();
      // The backend clears Claude Code's restored input after the interrupt.
      void terminalWrite(termId, '\x1b', false).catch(() => {});
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [convId]);

  const sendAsIs = useCallback(async (key: string, original: string) => {
    const termId = terminal?.terminalId;
    if (termId === undefined) return;
    const m = (useChatStore.getState().byConv[convId] ?? []).find((x) => x.key === key);
    useChatStore.getState().setWorking(convId, key, false);
    // A failed translation must not silently drop the attachments.
    try {
      await terminalSendPrompt(termId, withFiles(original, m?.files ?? []), m?.images ?? []);
    } catch (error) {
      useChatStore.getState().setError(convId, key, String(error));
    }
  }, [convId, terminal?.terminalId]);

  const retryTranslate = useCallback(async (key: string) => {
    const m = (useChatStore.getState().byConv[convId] ?? []).find((x) => x.key === key);
    if (!m) return;
    useChatStore.getState().setWorking(convId, key, true);
    try {
      const en = await useChatStore.getState().translateOutbound(convId, key, m.original);
      const termId = terminal?.terminalId;
      if (termId !== undefined) {
        await terminalSendPrompt(termId, withFiles(en, m.files ?? []), m.images ?? []);
      }
    } catch (e) {
      useChatStore.getState().setError(convId, key, String(e));
    }
  }, [convId, terminal?.terminalId]);

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
        {rows.map((row) => row.type === 'tools' ? (
          <ToolActivity key={row.key} messages={row.messages} />
        ) : (
          <MessageBubble
            key={row.key}
            msg={row.message}
            hideActivity={row.hideActivity}
            onRetry={retryTranslate}
            onSendAsIs={sendAsIs}
            planControls={row.message.key === pendingPlan?.messageKey ? planControls : undefined}
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
        {(images.length > 0 || files.length > 0) && (
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
            {files.map((f) => (
              <div className="attachment-file" key={f.id} title={f.path}>
                <FileIcon size={14} />
                <span className="attachment-name">{f.name}</span>
                <button
                  className="attachment-remove"
                  title="移除附件"
                  onClick={() => setFiles((prev) => prev.filter((x) => x.id !== f.id))}
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
            title="添加附件（图片或文件）"
            onClick={() => void pickAttachments()}
          >
            <PaperclipIcon size={17} />
          </button>
          <textarea
            ref={inputRef}
            value={input}
            rows={1}
            placeholder={
              exited !== undefined ? '终端已退出' : planBlocked ? '请先选择计划执行方式（草稿会保留）'
                : '输入中文（Enter 发送，Shift+Enter 换行，Esc 中断）'
            }
            disabled={exited !== undefined}
            onChange={(e) => setInput(e.target.value)}
            onPaste={onPaste}
            {...composerKeyboard}
          />
          <button
            className="send-btn"
            title="发送"
            onClick={() => void send()}
            disabled={planBlocked || sending || attaching > 0 || (!input.trim() && images.length === 0 && files.length === 0)}
          >
            <ArrowUpIcon size={16} />
          </button>
        </div>
      </div>
    </div>
  );
}
