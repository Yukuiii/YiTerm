import type { UpdateInfo } from './ipc';

const STARTUP_DELAY_MS = 2500;
export const UPDATE_INTERVAL_MS = 60 * 60 * 1000;

interface PendingCheck {
  manual: boolean;
  promise: Promise<UpdateInfo | null>;
}

/** Coordinates update requests and prompts for one running app instance. */
export function createUpdateChecker(
  fetchUpdate: () => Promise<UpdateInfo | null>,
  onInfo: (info: UpdateInfo | null) => void,
) {
  let running = false;
  let generation = 0;
  let pending: PendingCheck | null = null;
  let lastAttemptAt: number | null = null;
  let activeInfo: UpdateInfo | null = null;
  let startupTimer: number | undefined;
  let intervalTimer: number | undefined;
  const promptedVersions = new Set<string>();

  const check = (manual = false): Promise<UpdateInfo | null> => {
    if (!running) return Promise.reject(new Error('更新检查已停止。'));
    // Do not replace release information while its dialog may be downloading.
    if (activeInfo) return Promise.resolve(activeInfo);
    if (pending) {
      pending.manual ||= manual;
      return pending.promise;
    }

    lastAttemptAt = Date.now();
    const requestGeneration = generation;
    const request: PendingCheck = {
      manual,
      promise: Promise.resolve()
        .then(fetchUpdate)
        .then((info) => {
          if (
            running && generation === requestGeneration && info &&
            (request.manual || !promptedVersions.has(info.latestVersion))
          ) {
            activeInfo = info;
            promptedVersions.add(info.latestVersion);
            onInfo(info);
          }
          return info;
        })
        .finally(() => {
          // A stopped instance's request can finish after a newer one starts.
          if (pending === request) pending = null;
        }),
    };
    pending = request;
    return request.promise;
  };

  const automaticCheck = () => {
    // Network failures stay quiet and are retried on the next scheduled check.
    void check().catch(() => {});
  };

  const checkOnReturn = () => {
    if (
      document.visibilityState === 'visible' && lastAttemptAt !== null &&
      Date.now() - lastAttemptAt >= UPDATE_INTERVAL_MS
    ) automaticCheck();
  };

  const start = () => {
    if (running) return;
    running = true;
    startupTimer = window.setTimeout(() => {
      startupTimer = undefined;
      // A manual check can run before the startup delay elapses.
      if (lastAttemptAt === null || Date.now() - lastAttemptAt >= UPDATE_INTERVAL_MS) {
        automaticCheck();
      }
    }, STARTUP_DELAY_MS);
    intervalTimer = window.setInterval(automaticCheck, UPDATE_INTERVAL_MS);
    window.addEventListener('focus', checkOnReturn);
    document.addEventListener('visibilitychange', checkOnReturn);
  };

  const stop = () => {
    running = false;
    generation++;
    pending = null;
    if (startupTimer !== undefined) window.clearTimeout(startupTimer);
    if (intervalTimer !== undefined) window.clearInterval(intervalTimer);
    startupTimer = undefined;
    intervalTimer = undefined;
    window.removeEventListener('focus', checkOnReturn);
    document.removeEventListener('visibilitychange', checkOnReturn);
  };

  const dismiss = () => {
    activeInfo = null;
    if (running) onInfo(null);
  };

  return { start, stop, check, dismiss };
}
