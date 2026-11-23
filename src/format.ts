/** Last path segment, e.g. `/Users/me/code/yiterm` → `yiterm`. */
export function folderName(path: string): string {
  const trimmed = path.replace(/\/+$/, '');
  const parts = trimmed.split('/');
  return parts[parts.length - 1] || trimmed || '项目';
}

/** Collapses whitespace and truncates for compact single-line labels. */
export function oneLine(text: string, max = 60): string {
  const flat = text.replace(/\s+/g, ' ').trim();
  return flat.length > max ? `${flat.slice(0, max)}…` : flat;
}

/**
 * Drops Claude Code's `[Image #N]` markers from prompt text. The marker is
 * redundant wherever the thumbnail (or its absence) is already shown, and the
 * surrounding whitespace it leaves behind is collapsed.
 */
export function stripImageMarkers(text: string): string {
  return text.replace(/\[Image #\d+\]/g, ' ').replace(/\s+/g, ' ').trim();
}

/**
 * Friendly label for a transcript model id:
 * `claude-sonnet-4-5-20250929` → `Sonnet 4.5`,
 * `claude-3-5-haiku-20241022` → `Haiku 3.5`.
 * Non-Claude ids are returned as-is; unknowns fall back to the raw id.
 */
export function formatModel(model: string | null | undefined): string | null {
  const raw = model?.trim();
  if (!raw || raw === '<synthetic>') return null;
  if (!raw.toLowerCase().startsWith('claude-')) return raw;
  const parts = raw.replace(/-\d{8}$/, '').slice('claude-'.length).split('-');
  const family = parts.find((part) => /[a-z]/i.test(part));
  const version = parts.filter((part) => /^\d+$/.test(part)).slice(0, 2).join('.');
  if (!family) return raw;
  const name = family.charAt(0).toUpperCase() + family.slice(1);
  return version ? `${name} ${version}` : name;
}

/** `HH:mm` for a transcript timestamp; empty when the timestamp is unusable. */
export function formatClock(ts: string | null | undefined): string {
  if (!ts) return '';
  const date = new Date(ts);
  if (Number.isNaN(date.getTime())) return '';
  return `${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}`;
}

/**
 * True for turns that should play the entrance animation. Imported history is
 * older than the window, so replaying a conversation does not animate.
 */
export function isFreshTurn(
  ts: string | null | undefined,
  working?: boolean,
  now = Date.now(),
): boolean {
  if (working) return true;
  if (!ts) return false;
  const at = Date.parse(ts);
  return !Number.isNaN(at) && Math.abs(now - at) < 15_000;
}
