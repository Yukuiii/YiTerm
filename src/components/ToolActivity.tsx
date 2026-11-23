import { memo, useState, type ReactNode } from 'react';
import type { Msg, ToolCallItem } from '../stores/chat';
import { isFreshTurn, oneLine } from '../format';
import { summarizeToolActivity } from '../toolActivity';
import { LinkedText } from './ContentLink';
import CopyButton from './CopyButton';
import LiveDots from './LiveDots';
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

/**
 * Controlled disclosure with the reference's grid 0fr→1fr animation. While a
 * turn is live the content is forced open and the toggle is locked, so the
 * viewport does not jump as thinking streams in.
 */
function Disclosure({
  className,
  summary,
  children,
  live = false,
  copyText,
  copyLabel,
}: {
  className: string;
  summary: ReactNode;
  children: ReactNode;
  live?: boolean;
  copyText?: string;
  copyLabel?: string;
}) {
  const [open, setOpen] = useState(false);
  const expanded = live || open;
  const classes = ['disclosure', className, expanded ? 'is-open' : ''].filter(Boolean).join(' ');
  return (
    <div className={classes}>
      <div className="disclosure-head">
        <button
          type="button"
          className="disclosure-toggle"
          aria-expanded={expanded}
          disabled={live}
          onClick={() => setOpen((value) => !value)}
        >
          {summary}
        </button>
        {copyText && (
          <CopyButton
            text={copyText}
            className="disclosure-copy"
            label={copyLabel ?? '复制'}
            title={copyLabel ?? '复制'}
          />
        )}
      </div>
      <div className="disclosure-body">
        <div className="disclosure-inner" inert={!expanded}>
          {children}
        </div>
      </div>
    </div>
  );
}

function ThinkingLine({ thinking }: { thinking: string }) {
  const quote = thinking.split('\n').map((line) => `> ${line}`).join('\n');
  return (
    <Disclosure
      className="thinking-line"
      copyText={quote}
      copyLabel="复制引用"
      summary={(
        <>
          <span className="step-icon"><SparkleIcon size={14} /></span>
          <span className="step-label" title={thinking}>{oneLine(thinking, 96)}</span>
          <ChevronIcon size={13} className="step-chevron" />
        </>
      )}
    >
      <div className="step-body">
        <div className="thinking-body"><LinkedText>{thinking}</LinkedText></div>
      </div>
    </Disclosure>
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
    <Disclosure
      className={`step-item ${tool.isError ? 'is-error' : ''}`}
      summary={(
        <>
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
        </>
      )}
    >
      <div className="step-body">
        {detail && <pre className="step-command"><LinkedText>{detail}</LinkedText></pre>}
        {tool.result !== undefined ? (
          <pre className="step-output"><LinkedText>{tool.result || '（无输出）'}</LinkedText></pre>
        ) : (
          <div className="step-pending">等待输出…</div>
        )}
      </div>
    </Disclosure>
  );
}

export default memo(function ToolActivity({ messages, live = false }: {
  messages: Msg[];
  live?: boolean;
}) {
  const [enter] = useState(() => messages.some((message) => isFreshTurn(message.ts, message.working)));
  const calls = messages.flatMap((message) => message.toolCalls);
  const errors = calls.filter((tool) => tool.isError).length;
  const pending = calls.filter((tool) => tool.result === undefined).length;
  return (
    <Disclosure
      className={enter ? 'tool-activity chat-enter' : 'tool-activity'}
      live={live}
      summary={(
        <>
          {live
            ? <LiveDots />
            : <span className="tool-activity-dot" aria-hidden="true" />}
          <span className="tool-activity-label">{summarizeToolActivity(messages)}</span>
          {errors > 0 && <span className="tool-activity-status is-error">{errors} 项失败</span>}
          {pending > 0 && <span className="tool-activity-status">{pending} 项等待输出</span>}
          <ChevronIcon size={13} className="tool-activity-chevron" />
        </>
      )}
    >
      <div className="tool-activity-list">
        {messages.map((message) => (
          <div key={message.key} className="tool-activity-message">
            {message.thinking?.trim() && <ThinkingLine thinking={message.thinking.trim()} />}
            {message.toolCalls.map((tool) => <ToolStep key={tool.id} tool={tool} />)}
          </div>
        ))}
      </div>
    </Disclosure>
  );
}, (prev, next) => prev.live === next.live
  && prev.messages.length === next.messages.length
  && prev.messages.every((message, index) => message === next.messages[index]));
