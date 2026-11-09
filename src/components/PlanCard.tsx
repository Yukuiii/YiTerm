import type { PlanExecutionMode } from '../ipc';
import type { PlanApprovalState } from '../planApproval';
import type { Msg } from '../stores/chat';
import { FileIcon } from './Icons';
import Markdown from './Markdown';

export interface PlanControls {
  state: PlanApprovalState;
  onDecision: (mode: PlanExecutionMode) => Promise<boolean>;
  onOpenTerminal: () => void;
}

export default function PlanCard({ msg, content, controls }: {
  msg: Msg;
  content: string;
  controls?: PlanControls;
}) {
  const state = controls?.state;
  const available = state?.status?.available;
  const disabled = !available || state?.submitting || state?.sent;
  const decisionStatus = state?.submitting ? '正在提交执行选择…'
    : state?.sent ? '已发送执行选择，等待 Claude 确认…'
      : available ? '等待确认'
        : state?.checking ? '正在确认计划状态…' : '请在终端确认当前状态。';

  return (
    <article className="plan-card" aria-label="plan">
      <header className="plan-card-header">
        <FileIcon size={14} />
        <span>plan</span>
        {(msg.working || msg.error) && (
          <span className="plan-card-status" role="status" title={msg.error ?? undefined}>
            {msg.working ? '翻译中…' : '翻译失败，显示原文'}
          </span>
        )}
      </header>
      <div className="plan-card-body">
        <Markdown>{content}</Markdown>
      </div>
      {controls && state && (
        <footer className="plan-card-footer">
          <div className="plan-decision-status" role="status">{decisionStatus}</div>
          {state.error && <div className="plan-decision-error" role="alert">{state.error}</div>}
          <div className="plan-decision-actions">
            {state.status?.modes.includes('manual') && (
              <button
                type="button"
                className="primary-btn"
                disabled={disabled}
                onClick={() => void controls.onDecision('manual')}
              >
                执行，逐项确认权限
              </button>
            )}
            {state.status?.modes.includes('bypass') && (
              <button
                type="button"
                className="plan-decision-secondary"
                disabled={disabled}
                onClick={() => void controls.onDecision('bypass')}
              >
                执行，本会话跳过权限确认
              </button>
            )}
            <button type="button" className="plan-decision-secondary" onClick={controls.onOpenTerminal}>
              在终端修改计划
            </button>
          </div>
        </footer>
      )}
    </article>
  );
}
