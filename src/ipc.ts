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
}

export interface TranslationSettings {
  mode: 'google' | 'chat';
  targetZh: string;
  chatBaseUrl: string | null;
  chatModelId: string | null;
}

export interface Settings {
  translation: TranslationSettings;
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
}

export interface SessionUpdate {
  conversationId: string;
  sessionId: string;
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

export const conversationsList = () => invoke<Conversation[]>('conversations_list');

export const conversationsCreate = (folder: string, title?: string) =>
  invoke<Conversation>('conversations_create', { folder, title: title ?? null });

export const conversationsDelete = (id: string) =>
  invoke<void>('conversations_delete', { id });

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

// ---------- settings ----------

export const settingsGet = () => invoke<Settings>('settings_get');

export const settingsSet = (settings: Settings) =>
  invoke<void>('settings_set', { settings });

export const apiKeyGet = () => invoke<string | null>('api_key_get');

export const apiKeySet = (key: string | null) => invoke<void>('api_key_set', { key });

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
): Promise<string> {
  return new Promise<string>((resolve, reject) => {
    const channel = new Channel<TranslateEvent>();
    let timer: number | undefined;
    const arm = () => {
      if (timer !== undefined) window.clearTimeout(timer);
      timer = window.setTimeout(
        () => reject(new Error('翻译服务无响应（超时），请检查网络或翻译设置')),
        idleTimeoutMs,
      );
    };
    channel.onmessage = (ev) => {
      arm();
      onUpdate(ev);
    };
    arm();
    const requestId = crypto.randomUUID();
    invoke<string>('translate', { requestId, direction, text, channel })
      .then((final) => {
        if (timer !== undefined) window.clearTimeout(timer);
        resolve(final);
      })
      .catch((e) => {
        if (timer !== undefined) window.clearTimeout(timer);
        reject(e);
      });
  });
}

// ---------- events ----------

export function onSessionUpdate(cb: (payload: SessionUpdate) => void): Promise<UnlistenFn> {
  return listen<SessionUpdate>('session-update', (e) => cb(e.payload));
}

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
