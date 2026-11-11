import type { GitCommitDetails } from './ipc';

/** Commit contents are immutable. Reuse successful/in-flight reads while the
 * panel is mounted, and discard failures so the card's retry can query again.
 */
export function createCommitDetailsCache(load: (oid: string) => Promise<GitCommitDetails>) {
  const cache = new Map<string, Promise<GitCommitDetails>>();
  return (oid: string) => {
    const existing = cache.get(oid);
    if (existing) return existing;
    const pending = load(oid).catch((error: unknown) => {
      if (cache.get(oid) === pending) cache.delete(oid);
      throw error;
    });
    cache.set(oid, pending);
    if (cache.size > 100) cache.delete(cache.keys().next().value!);
    return pending;
  };
}

export function formatCommitDate(seconds: number) {
  const date = new Date(seconds * 1000);
  return Number.isNaN(date.getTime()) ? '未知时间' : date.toLocaleString('zh-CN', {
    dateStyle: 'long', timeStyle: 'short', hour12: false,
  });
}

export function formatCommitAge(seconds: number, now = Date.now()) {
  const elapsed = seconds - now / 1000;
  if (!Number.isFinite(elapsed)) return '未知时间';
  if (Math.abs(elapsed) < 60) return elapsed > 0 ? '即将' : '刚刚';
  const units: [Intl.RelativeTimeFormatUnit, number][] = [
    ['year', 365 * 86400], ['month', 30 * 86400], ['day', 86400], ['hour', 3600], ['minute', 60],
  ];
  const [unit, duration] = units.find(([, duration]) => Math.abs(elapsed) >= duration)!;
  return new Intl.RelativeTimeFormat('zh-CN', { numeric: 'always' }).format(Math.trunc(elapsed / duration), unit);
}
