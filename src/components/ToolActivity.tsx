import { memo } from 'react';
import type { Msg, ToolCallItem } from '../stores/chat';
import { oneLine } from '../format';
import { summarizeToolActivity } from '../toolActivity';
import { LinkedText } from './ContentLink';
import { ChevronIcon, FileIcon, SearchIcon, SparkleIcon, TerminalIcon, WrenchIcon } from './Icons';

/** One-line verb for tool steps whose label is a bare path/pattern/url. */
const TOOL_ACTION: Record<string, string> = {
  Read: '读取',
  Write: '写入',
  Edit: '编辑',
  MultiEdit: '编辑',
  NotebookEdit: '编辑',
  Grep: '搜索',
  Glob: '查找',
  WebFetch: '抓取',
  WebSearch: '搜索',
  Task: '子任务',
};

function ThinkingLine({ thinking }: { thinking: string }) {
  return (
    <details className="thinking-line">
      <summary>
        <span className="step-icon"><SparkleIcon size={14} /></span>
        <span className="step-label" title={thinking}>{oneLine(thinking, 96)}</span>
        <ChevronIcon size={13} className="step-chevron" />
      </summary>
      <div className="step-body">
        <div className="thinking-body"><LinkedText>{thinking}</LinkedText></div>
      </div>
    </details>
  );
}

function ToolStep({ tool }: { tool: ToolCallItem }) {
  // Older caches used an empty input object as the tool label/detail.
  const detail = tool.detail === '{}' ? '' : tool.detail;
  const isPlanUpdate = ['Write', 'Edit', 'MultiEdit'].includes(tool.name)
    && /(?:^|[/\\])\.claude[/\\]plans[/\\][^/\\]+\.md$/.test(detail);
  const StepIcon = ['Read', 'Write', 'Edit', 'MultiEdit', 'NotebookEdit', 'EnterPlanMode'].includes(tool.name)
    ? FileIcon : tool.name === 'Bash' ? TerminalIcon
      : ['Grep', 'Glob', 'WebSearch'].includes(tool.name) ? SearchIcon : WrenchIcon;
  const label = isPlanUpdate ? 'Update Plan'
    : tool.name === 'EnterPlanMode' ? 'Enter Plan Mode'
      : (tool.label || detail || tool.name);
  const action = isPlanUpdate || label === tool.name ? undefined : TOOL_ACTION[tool.name];
  return (
    <details className={`step-item ${tool.isError ? 'is-error' : ''}`}>
      <summary>
        <span className="step-icon"><StepIcon size={14} /></span>
        <span className="step-label" title={detail || tool.name}>
          {action && <span className="step-action">{action}</span>}
          {label}
        </span>
        {tool.result !== undefined && (
          <span className="step-status" aria-label={tool.isError ? '失败' : '已完成'}>
            {tool.isError ? '失败' : '✓'}
          </span>
        )}
        <ChevronIcon size={13} className="step-chevron" />
      </summary>
      <div className="step-body">
        {detail && <pre className="step-command"><LinkedText>{detail}</LinkedText></pre>}
        {tool.result !== undefined ? (
          <pre className="step-output"><LinkedText>{tool.result || '（无输出）'}</LinkedText></pre>
        ) : (
          <div className="step-pending">等待输出…</div>
        )}
      </div>
    </details>
  );
}

export default memo(function ToolActivity({ messages }: { messages: Msg[] }) {
  const calls = messages.flatMap((message) => message.toolCalls);
  const errors = calls.filter((tool) => tool.isError).length;
  const pending = calls.filter((tool) => tool.result === undefined).length;
  return (
    <details className="tool-activity">
      <summary className="tool-activity-summary">
        <span className="tool-activity-label">{summarizeToolActivity(messages)}</span>
        <ChevronIcon size={13} className="tool-activity-chevron" />
        {errors > 0 && <span className="tool-activity-status is-error">{errors} 项失败</span>}
        {pending > 0 && <span className="tool-activity-status">{pending} 项等待输出</span>}
      </summary>
      <div className="tool-activity-list">
        {messages.map((message) => (
          <div key={message.key} className="tool-activity-message">
            {message.thinking?.trim() && <ThinkingLine thinking={message.thinking.trim()} />}
            {message.toolCalls.map((tool) => <ToolStep key={tool.id} tool={tool} />)}
          </div>
        ))}
      </div>
    </details>
  );
}, (prev, next) => prev.messages.length === next.messages.length
  && prev.messages.every((message, index) => message === next.messages[index]));
