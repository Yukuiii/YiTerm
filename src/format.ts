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
