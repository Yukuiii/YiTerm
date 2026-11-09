import type { PlanApprovalStatus, PlanExecutionMode } from './ipc';
import type { Msg } from './stores/chat';

export interface PlanApprovalState {
  status: PlanApprovalStatus | null;
  checking: boolean;
  submitting: boolean;
  sent: boolean;
  error: string | null;
}

export const EMPTY_PLAN_APPROVAL: PlanApprovalState = {
  status: null, checking: false, submitting: false, sent: false, error: null,
};

/** A settled newer plan must never expose the controls of an older pending one. */
export function latestPlanCall(messages: Msg[]) {
  for (let i = messages.length - 1; i >= 0; i--) {
    const message = messages[i];
    if (message.role !== 'assistant') continue;
    const call = message.toolCalls.findLast((tool) => tool.name === 'ExitPlanMode');
    if (call) return { messageKey: message.key, callId: call.id, resolved: call.result !== undefined };
  }
  return null;
}

function sameStatus(a: PlanApprovalStatus | null, b: PlanApprovalStatus | null) {
  return a === b || (!!a && !!b && a.available === b.available && a.token === b.token &&
    a.modes.length === b.modes.length && a.modes.every((mode, i) => mode === b.modes[i]));
}

/** Ephemeral polling and submission state for one exact plan/terminal context. */
export function createPlanApprovalController(
  fetchStatus: () => Promise<PlanApprovalStatus>,
  sendDecision: (token: number, mode: PlanExecutionMode) => Promise<void>,
  onState: (state: PlanApprovalState) => void,
) {
  let state: PlanApprovalState = { ...EMPTY_PLAN_APPROVAL, checking: true };
  let running = false;
  let generation = 0;
  let query: object | null = null;
  let timer: number | undefined;
  let decisionError: string | null = null;

  const publish = (patch: Partial<PlanApprovalState>) => {
    const next = { ...state, ...patch };
    if (sameStatus(next.status, state.status) && next.checking === state.checking &&
      next.submitting === state.submitting && next.sent === state.sent && next.error === state.error) return;
    state = next;
    if (running) onState(state);
  };

  const refresh = async () => {
    if (!running || query || state.submitting || state.sent || document.visibilityState !== 'visible') return;
    const request = {};
    const requestGeneration = generation;
    query = request;
    try {
      const status = await fetchStatus();
      if (running && generation === requestGeneration) {
        publish({ status, checking: false, error: decisionError });
      }
    } catch {
      if (running && generation === requestGeneration) {
        publish({ status: null, checking: false, error: decisionError ?? '暂时无法确认计划状态，请在终端继续。' });
      }
    } finally {
      if (query === request) query = null;
    }
  };

  const decide = async (mode: PlanExecutionMode): Promise<boolean> => {
    if (!running || state.submitting || state.sent) return false;
    const status = state.status;
    if (!status?.available || status.token === null || !status.modes.includes(mode)) return false;
    const requestGeneration = ++generation;
    decisionError = null;
    publish({ submitting: true, checking: false, error: null });
    try {
      await sendDecision(status.token, mode);
      if (running && generation === requestGeneration) {
        publish({ submitting: false, sent: true, status: null });
      }
      return true;
    } catch {
      if (running && generation === requestGeneration) {
        decisionError = '执行选择未能确认，请在终端查看当前状态。';
        publish({ submitting: false, status: null, error: decisionError });
        void refresh();
      }
      return false;
    }
  };

  const onReturn = () => { void refresh(); };
  const start = () => {
    if (running) return;
    running = true;
    state = { ...state, status: null, checking: !state.sent, submitting: false };
    onState(state);
    void refresh();
    timer = window.setInterval(onReturn, 500);
    window.addEventListener('focus', onReturn);
    document.addEventListener('visibilitychange', onReturn);
  };
  const stop = () => {
    running = false;
    generation++;
    query = null;
    if (timer !== undefined) window.clearInterval(timer);
    timer = undefined;
    window.removeEventListener('focus', onReturn);
    document.removeEventListener('visibilitychange', onReturn);
  };

  return { start, stop, refresh, decide };
}
