import { useCallback, useLayoutEffect, useMemo, useState } from 'react';
import { planApprovalDecide, planApprovalStatus, type PlanExecutionMode } from '../ipc';
import { createPlanApprovalController, EMPTY_PLAN_APPROVAL, type PlanApprovalState } from '../planApproval';

export function usePlanApproval(conversationId: string, terminalId: number | undefined, callId: string | undefined) {
  const [snapshot, setSnapshot] = useState<{ owner: object; state: PlanApprovalState } | null>(null);
  const controller = useMemo(() => {
    if (terminalId === undefined || !callId) return null;
    const owner = {};
    return {
      owner,
      value: createPlanApprovalController(
        () => planApprovalStatus(conversationId, terminalId, callId),
        (token, mode) => planApprovalDecide(conversationId, terminalId, callId, token, mode),
        (state) => setSnapshot({ owner, state }),
      ),
    };
  }, [conversationId, terminalId, callId]);

  // Retire the previous context at commit, before its controls can receive events.
  useLayoutEffect(() => {
    controller?.value.start();
    return () => controller?.value.stop();
  }, [controller]);

  const decide = useCallback((mode: PlanExecutionMode) =>
    controller?.value.decide(mode) ?? Promise.resolve(false), [controller]);

  const state = controller && snapshot?.owner === controller.owner
    ? snapshot.state : { ...EMPTY_PLAN_APPROVAL, checking: controller !== null };
  return { state, decide };
}
