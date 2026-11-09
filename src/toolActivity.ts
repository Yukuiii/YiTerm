import type { Msg } from './stores/chat';

export type ChatRow =
  | { type: 'message'; key: string; message: Msg; hideActivity?: boolean }
  | { type: 'tools'; key: string; messages: Msg[] };

/** Keep transcript records intact; only combine adjacent activity for display. */
export function groupToolActivity(messages: Msg[]): ChatRow[] {
  const rows: ChatRow[] = [];
  let activity: Msg[] = [];
  const flush = () => {
    if (activity.length === 0) return;
    rows.push({ type: 'tools', key: `tools:${activity[0].key}`, messages: activity });
    activity = [];
  };

  for (const message of messages) {
    const hasActivity = message.toolCalls.length > 0 || !!message.thinking?.trim();
    const hasBody = !!message.original.trim() || !!message.translation?.trim()
      || message.working || !!message.error
      || !!message.images?.length || !!message.files?.length || !!message.hadImages;
    const isPlan = message.toolCalls.some((tool) => tool.name === 'ExitPlanMode');
    if (message.role === 'assistant' && hasActivity && !isPlan) {
      activity.push(message);
      // Thinking and tools precede the body even when Claude puts them in the
      // same message. Fold them into the preceding activity before the reply.
      if (hasBody) {
        flush();
        rows.push({ type: 'message', key: message.key, message, hideActivity: true });
      }
    } else {
      flush();
      rows.push({ type: 'message', key: message.key, message });
    }
  }
  flush();
  return rows;
}

export function summarizeToolActivity(messages: Msg[]): string {
  const thoughts = messages.filter((message) => message.thinking?.trim()).length;
  const calls = messages.flatMap((message) => message.toolCalls);
  const reads = calls.filter((tool) => tool.name === 'Read');
  const others = calls.filter((tool) => tool.name !== 'Read');
  const parts: string[] = [];
  if (thoughts > 0) parts.push(`思考了 ${thoughts} 次`);
  if (reads.length > 0) {
    const files = new Set(reads.map((tool) => tool.detail && tool.detail !== '{}' ? tool.detail : tool.id));
    const complete = reads.every((tool) => tool.result !== undefined && !tool.isError);
    parts.push(`读取${complete ? '了' : ''} ${files.size} 个文件`);
  }
  if (others.length > 0) {
    const complete = others.every((tool) => tool.result !== undefined);
    parts.push(`使用${complete ? '了' : ''} ${others.length} 个工具`);
  }
  return parts.join('，');
}
