import { invoke } from '@tauri-apps/api/core';
import { Channel } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

// ---------- shared types (mirror Rust serde camelCase) ----------

export interface Conversation {
  id: string;
  folder: string;
  sessionId: string;
  terminalId: number | null;
  title: string;
  createdAt: string;
  /** Discovered from Claude Code's records rather than created in YiTerm. */
  imported?: boolean;
}

export interface ConversationsOverview {
  conversations: Conversation[];
  projects: string[];
}

export interface TranslationSettings {
  mode: 'google' | 'dlx' | 'chat' | 'local' | 'deepl';
  targetZh: string;
  chatProvider: 'custom' | 'siliconflow' | 'openrouter';
  chatBaseUrl: string | null;
  chatModelId: string | null;
  deeplApiUrl: string | null;
  deeplProtocol: 'deepl' | 'deeplx' | null;
}

export interface TerminalSettings {
  fontFamily: string | null;
  fontSize: number;
  lineHeight: number;
}

export interface Settings {
  translation: TranslationSettings;
  terminal: TerminalSettings;
  claudePath: string | null;
}

export type Direction = 'zh-en' | 'en-zh';

export interface ToolCall {
  id: string;
  name: string;
  /** Human-readable one-line label (falls back to detail for old caches). */
  label?: string;
  detail: string;
}

export interface ToolResult {
  callId: string;
  isError: boolean;
  text: string;
}

export interface ChatMsg {
  key: string;
  role: 'user' | 'assistant';
  text: string;
  thinking: string | null;
  toolCalls: ToolCall[];
  toolResult: ToolResult | null;
  ts: string | null;
  done: boolean;
  /** The prompt was interrupted before any assistant reply. */
  interrupted?: boolean;
}

export interface SessionUpdate {
  conversationId: string;
  sessionId: string;
  /** The conversation came from Claude's records, not YiTerm. */
  imported: boolean;
  /** The batch is transcript history replayed on attach, not live activity. */
  historical: boolean;
  upserts: ChatMsg[];
}

export type TerminalEvent =
  | { type: 'data'; seq: number; b64: string; reset: boolean }
  | { type: 'exit'; code: number | null };

export type TranslateEvent =
  | { type: 'delta'; requestId: string; text: string }
  | { type: 'done'; requestId: string; text: string }
  | { type: 'error'; requestId: string; message: string };

export interface TerminalStartInfo {
  conversationId: string;
  terminalId: number;
  sessionId: string;
  command: string;
  resumed: boolean;
  /** Conversations whose terminals were reclaimed to stay under the cap. */
  evictedConversationIds: string[];
}

// ---------- terminal ----------

export const terminalStart = (conversationId: string) =>
  invoke<TerminalStartInfo>('terminal_start', { req: { conversationId } });

export const terminalWrite = (id: number, text: string, submit: boolean) =>
  invoke<void>('terminal_write', { id, text, submit });

/**
 * Sends one composer turn. Image paths are pasted into Claude Code and given
 * time to attach before Enter is pressed; the backend owns that sequencing.
 */
export const terminalSendPrompt = (id: number, text: string, imagePaths: string[] = []) =>
  invoke<void>('terminal_send_prompt', { id, text, imagePaths });

export const terminalAttach = (id: number, seq: number, channel: Channel<TerminalEvent>) =>
  invoke<void>('terminal_attach', { id, seq, channel });

export const terminalResize = (id: number, cols: number, rows: number) =>
  invoke<void>('terminal_resize', { id, cols, rows });

export const terminalKill = (id: number) => invoke<void>('terminal_kill', { id });

// ---------- conversations ----------

/** YiTerm conversations plus Claude Code's projects and session transcripts. */
export const conversationsList = () => invoke<ConversationsOverview>('conversations_list');

export const conversationsCreate = (folder: string, title?: string) =>
  invoke<Conversation>('conversations_create', { folder, title: title ?? null });

// ---------- Git changes ----------

export type GitArea = 'staged' | 'unstaged';

export interface GitFileChange {
  path: string;
  originalPath: string | null;
  status: string;
  conflicted: boolean;
}

export interface GitStatus {
  root: string | null;
  staged: GitFileChange[];
  unstaged: GitFileChange[];
}

export interface GitDiff {
  text: string;
  kind: 'text' | 'binary' | 'conflict' | 'empty';
  truncated: boolean;
}

export const gitStatus = (folder: string) => invoke<GitStatus>('git_status', { folder });

export const gitDiff = (folder: string, area: GitArea, path: string) =>
  invoke<GitDiff>('git_diff', { folder, area, path });

// ---------- diagnostics ----------

export const debugLog = (message: string) =>
  invoke<void>('debug_log', { message }).catch(() => {});

export const currentUser = () => invoke<string>('current_user');

// ---------- chat history persistence ----------

export const chatSave = (convId: string, data: string) =>
  invoke<void>('chat_save', { convId, data });

export const chatLoad = (convId: string) =>
  invoke<string | null>('chat_load', { convId });

/** Persists a pasted image; returns the absolute path to hand to Claude Code. */
export const chatImageSave = (convId: string, mime: string, data: string) =>
  invoke<string>('chat_image_save', { convId, mime, data });

export interface ImportedAttachment {
  kind: 'image' | 'file';
  path: string;
  name: string;
}

/** Copies an image into the cache or returns the file's original path. */
export const chatAttachmentImport = (convId: string, path: string) =>
  invoke<ImportedAttachment>('chat_attachment_import', { convId, path });

// ---------- settings ----------

export const settingsGet = () => invoke<Settings>('settings_get');

export const settingsSet = (settings: Settings) =>
  invoke<void>('settings_set', { settings });

export const apiKeyGet = (provider: string) =>
  invoke<string | null>('api_key_get', { provider });

export const apiKeySet = (key: string | null, provider: string) =>
  invoke<void>('api_key_set', { key, provider });

// ---------- local model ----------

export interface ModelStatus {
  state: 'missing' | 'downloading' | 'ready' | 'error';
  downloaded: number;
  total: number;
  path: string;
  message: string | null;
}

export interface ModelProgress {
  state: 'downloading' | 'ready' | 'error';
  downloaded: number;
  total: number;
  message: string | null;
}

export const modelStatus = () => invoke<ModelStatus>('model_status');

/** Starts (or resumes) the background download; progress via onModelProgress. */
export const modelDownload = () => invoke<void>('model_download');

export const modelDelete = () => invoke<void>('model_delete');

export function onModelProgress(cb: (p: ModelProgress) => void): Promise<UnlistenFn> {
  return listen<ModelProgress>('model-progress', (e) => cb(e.payload));
}

// ---------- translate ----------

/**
 * Runs one translation, streaming incremental deltas into the channel.
 * Resolves with the final full text (the Done payload).
 *
 * A watchdog rejects if no event arrives for `idleTimeoutMs`, so a hung
 * network path can never leave the UI stuck on “翻译中…” forever.
 */
export function translate(
  direction: Direction,
  text: string,
  onUpdate: (ev: TranslateEvent) => void,
  idleTimeoutMs = 25_000,
  signal?: AbortSignal,
): Promise<string> {
  return new Promise<string>((resolve, reject) => {
    const requestId = crypto.randomUUID();
    const channel = new Channel<TranslateEvent>();
    let timer: number | undefined;
    let aborted = false;

    const onAbort = () => {
      aborted = true;
      if (timer !== undefined) window.clearTimeout(timer);
      void invoke('translate_cancel', { requestId }).catch(() => {});
      reject(new DOMException('翻译已取消', 'AbortError'));
    };
    const settle = () => {
      if (timer !== undefined) window.clearTimeout(timer);
      signal?.removeEventListener('abort', onAbort);
    };
    const arm = () => {
      if (timer !== undefined) window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        aborted = true;
        settle();
        void invoke('translate_cancel', { requestId }).catch(() => {});
        reject(new Error('翻译服务无响应（超时），请检查网络或翻译设置'));
      }, idleTimeoutMs);
    };
    channel.onmessage = (ev) => {
      if (aborted) return;
      arm();
      onUpdate(ev);
    };
    arm();
    if (signal) {
      if (signal.aborted) {
        onAbort();
        return;
      }
      signal.addEventListener('abort', onAbort, { once: true });
    }
    invoke<string>('translate', { requestId, direction, text, channel })
      .then((final) => {
        if (aborted) return;
        settle();
        resolve(final);
      })
      .catch((e) => {
        if (aborted) return;
        settle();
        reject(e);
      });
  });
}

// ---------- events ----------

export function onSessionUpdate(cb: (payload: SessionUpdate) => void): Promise<UnlistenFn> {
  return listen<SessionUpdate>('session-update', (e) => cb(e.payload));
}

/** A conversation's turn started or finished (drives the sidebar spinner). */
export interface ConversationBusy {
  conversationId: string;
  busy: boolean;
}

export function onConversationBusy(cb: (p: ConversationBusy) => void): Promise<UnlistenFn> {
  return listen<ConversationBusy>('conversation-busy', (e) => cb(e.payload));
}

/** Esc aborted a running turn before any assistant reply. */
export interface ConversationInterrupted {
  conversationId: string;
}

export function onConversationInterrupted(
  cb: (p: ConversationInterrupted) => void,
): Promise<UnlistenFn> {
  return listen<ConversationInterrupted>('conversation-interrupted', (e) => cb(e.payload));
}

// ---------- updates ----------

export interface UpdateInfo {
  currentVersion: string;
  latestVersion: string;
  name: string;
  notes: string;
  assetName: string;
  assetUrl: string;
  publishedAt: string | null;
}

export type UpdateProgress =
  | { type: 'progress'; downloaded: number; total: number }
  | { type: 'done'; path: string }
  | { type: 'error'; message: string };

export const updateCheck = () => invoke<UpdateInfo | null>('update_check');

export function updateDownload(
  url: string,
  onProgress: (p: UpdateProgress) => void,
): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    const channel = new Channel<UpdateProgress>();
    channel.onmessage = (p) => {
      onProgress(p);
      if (p.type === 'done') resolve();
      else if (p.type === 'error') reject(new Error(p.message));
    };
    invoke<void>('update_download', { url, channel }).catch((e) => reject(e));
  });
}

export const updateInstall = () => invoke<void>('update_install');

// ---------- misc ----------

export function hasCJK(text: string): boolean {
  return /[぀-ヿ㐀-䶿一-鿿豈-﫿가-힯ᄀ-ᇿ]/.test(text);
}

/** Base64-encodes bytes for the JSON IPC. Chunked: `String.fromCharCode(...bytes)`
 *  overflows the argument limit on a multi-MB screenshot. */
export function encodeBytesToBase64(bytes: Uint8Array): string {
  const CHUNK = 0x8000;
  let bin = '';
  for (let i = 0; i < bytes.length; i += CHUNK) {
    bin += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  }
  return btoa(bin);
}

export function decodeBase64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}
