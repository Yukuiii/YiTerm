import { useCallback, useEffect, useRef, useState } from 'react';
import { gitStatus, type GitStatus } from '../ipc';

export interface GitStatusState {
  status: GitStatus | null;
  error: string | null;
  revision: number;
  refresh: () => void;
}

interface Snapshot {
  folder: string | null;
  status: GitStatus | null;
  error: string | null;
  revision: number;
}

/** The status bar and changes panel share one read of the current project. */
export function useGitStatus(folder: string | null): GitStatusState {
  const [snapshot, setSnapshot] = useState<Snapshot>({ folder: null, status: null, error: null, revision: 0 });
  const refreshRef = useRef<() => void>(() => {});
  const refresh = useCallback(() => refreshRef.current(), []);
  useEffect(() => {
    if (!folder) return;
    const request = { disposed: false, running: false };
    const read = async () => {
      if (request.disposed || request.running || document.visibilityState !== 'visible') return;
      request.running = true;
      try {
        const status = await gitStatus(folder);
        if (!request.disposed) {
          setSnapshot((previous) => ({
            folder, status, error: null,
            // XY may stay unchanged while the selected file's contents change.
            revision: previous.folder === folder ? previous.revision + 1 : 1,
          }));
        }
      } catch (error) {
        if (!request.disposed) {
          setSnapshot((previous) => ({
            folder, error: String(error),
            status: previous.folder === folder ? previous.status : null,
            revision: previous.folder === folder ? previous.revision : 0,
          }));
        }
      } finally {
        request.running = false;
      }
    };
    const readVisible = () => { void read(); };
    refreshRef.current = readVisible;
    readVisible();
    const timer = window.setInterval(readVisible, 5000);
    window.addEventListener('focus', readVisible);
    document.addEventListener('visibilitychange', readVisible);
    return () => {
      request.disposed = true;
      refreshRef.current = () => {};
      window.clearInterval(timer);
      window.removeEventListener('focus', readVisible);
      document.removeEventListener('visibilitychange', readVisible);
    };
  }, [folder]);
  // Do not show the previous project's branch while its successor is loading.
  return snapshot.folder === folder
    ? { status: snapshot.status, error: snapshot.error, revision: snapshot.revision, refresh }
    : { status: null, error: null, revision: 0, refresh };
}
