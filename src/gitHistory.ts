import type { GitHistory } from './ipc';

export const HISTORY_PAGE_SIZE = 100;
export const HISTORY_LIMIT = 1000;

export interface HistoryState {
  history: GitHistory | null;
  loading: boolean;
  error: string | null;
}

/** One in-flight read per mounted panel. Urgent requests coalesce into a read
 * of the latest limit; periodic ticks can be skipped while Git is running.
 */
export function createGitHistoryLoader(
  load: (limit: number) => Promise<GitHistory>,
  publish: (state: HistoryState) => void,
) {
  let state: HistoryState = { history: null, loading: false, error: null };
  let desiredLimit = HISTORY_PAGE_SIZE;
  let loadedLimit = HISTORY_PAGE_SIZE;
  let expanded = true;
  let running = false;
  let pending = false;
  let disposed = false;
  let generation = 0;
  const visible = () => document.visibilityState === 'visible';
  const update = (patch: Partial<HistoryState>) => {
    state = { ...state, ...patch };
    publish(state);
  };
  const refresh = async (urgent = true) => {
    if (disposed || !expanded || !visible()) return;
    if (running) {
      if (urgent) {
        pending = true;
        update({ loading: true });
      }
      return;
    }
    running = true;
    const requestGeneration = generation;
    const limit = desiredLimit;
    update({ loading: true, error: null });
    try {
      const history = await load(limit);
      if (!disposed && generation === requestGeneration) {
        loadedLimit = limit;
        update({ history, error: null });
      }
    } catch (error) {
      if (!disposed && generation === requestGeneration) update({ error: String(error) });
    } finally {
      running = false;
      if (!disposed && generation === requestGeneration) update({ loading: false });
      if (pending) {
        pending = false;
        void refresh();
      }
    }
  };
  const invalidate = () => {
    generation++;
    pending = false;
    update({ loading: false });
  };
  const onFocus = () => { void refresh(); };
  const onVisibility = () => {
    if (visible()) void refresh();
    else invalidate();
  };
  const timer = window.setInterval(() => { void refresh(false); }, 5000);
  window.addEventListener('focus', onFocus);
  document.addEventListener('visibilitychange', onVisibility);
  void refresh();
  return {
    refresh: () => { void refresh(); },
    setExpanded(value: boolean) {
      if (expanded === value || disposed) return;
      expanded = value;
      invalidate();
      if (expanded) void refresh();
    },
    loadMore() {
      if (disposed || state.loading || !state.history?.hasMore || loadedLimit >= HISTORY_LIMIT) return;
      desiredLimit = Math.min(loadedLimit + HISTORY_PAGE_SIZE, HISTORY_LIMIT);
      generation++;
      void refresh();
    },
    stop() {
      disposed = true;
      generation++;
      window.clearInterval(timer);
      window.removeEventListener('focus', onFocus);
      document.removeEventListener('visibilitychange', onVisibility);
    },
  };
}
