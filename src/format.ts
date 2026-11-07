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
