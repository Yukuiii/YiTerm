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

export interface ChatMsg {
  key: string;
  role: 'user' | 'assistant';
  text: string;
  thinking: string | null;
  tools: string[];
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
 */
export async function translate(
  direction: Direction,
  text: string,
  onUpdate: (ev: TranslateEvent) => void,
): Promise<string> {
  const channel = new Channel<TranslateEvent>();
  channel.onmessage = onUpdate;
  const requestId = crypto.randomUUID();
  return invoke<string>('translate', {
    requestId,
    direction,
    text,
    channel,
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

export function decodeBase64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}
