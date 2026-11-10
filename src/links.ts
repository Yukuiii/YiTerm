/** Schemes supported by the desktop opener's existing default permission. */
export function externalUrl(href: string | undefined): string | null {
  if (!href) return null;
  const value = href.trim();
  const absolute = /^www\./i.test(value) ? `https://${value}`
    : value.startsWith('//') ? `https:${value}` : value;
  try {
    const url = new URL(absolute);
    return ['http:', 'https:', 'mailto:', 'tel:'].includes(url.protocol) ? url.href : null;
  } catch {
    return null;
  }
}

export interface FileReference {
  path: string;
  line?: number;
  column?: number;
}

/** Claude's local Markdown targets commonly include :line[:column] or #Lline. */
export function fileReference(href: string | undefined): FileReference | null {
  if (!href || /[\0\r\n]/.test(href)) return null;
  let value = href.trim();
  let fragment = '';
  if (/^file:/i.test(value)) {
    try {
      const url = new URL(value);
      if (url.hostname && url.hostname !== 'localhost') return null;
      value = url.pathname;
      fragment = url.hash;
    } catch { return null; }
  } else {
    const hash = value.indexOf('#');
    if (hash !== -1) { fragment = value.slice(hash); value = value.slice(0, hash); }
  }
  const position = /:(\d+)(?::(\d+))?$/.exec(value);
  if (position) value = value.slice(0, position.index);
  const hashPosition = /^#L(\d+)(?:C(\d+))?$/i.exec(fragment);
  if (/^[a-z][a-z\d+.-]*:/i.test(value) || value.startsWith('//') || value.includes('?')) return null;
  try { value = decodeURIComponent(value); } catch { return null; }
  if (!value || /[\0\r\n]/.test(value)) return null;
  const pathLike = value.includes('/') || /^(?:[\w.-]+\.[a-z\d]+|\.[\w.-]+|Dockerfile|Makefile|LICENSE|README)$/i.test(value);
  if (!pathLike) return null;
  const coordinates = hashPosition ?? position;
  const line = coordinates ? Number(coordinates[1]) : undefined;
  const column = coordinates?.[2] ? Number(coordinates[2]) : undefined;
  if ((line !== undefined && (!Number.isSafeInteger(line) || line < 1))
    || (column !== undefined && (!Number.isSafeInteger(column) || column < 1))) return null;
  return { path: value, line, column };
}

export interface LinkPart {
  text: string;
  href?: string;
}

/** Keep prose punctuation outside links, retaining balanced URL parentheses. */
function trimUrl(value: string): string {
  const pairs: Record<string, string> = { '(': ')', '[': ']', '{': '}' };
  const extra: Record<string, number> = { ')': 0, ']': 0, '}': 0 };
  for (const char of value) {
    if (pairs[char]) extra[pairs[char]]--;
    else if (char in extra) extra[char]++;
  }
  let end = value.length;
  while (end > 0) {
    const char = value[end - 1];
    if ('.,!?;:'.includes(char)) end--;
    else if (extra[char] > 0) { extra[char]--; end--; }
    else break;
  }
  return value.slice(0, end);
}

/**
 * Linkify bare web URLs without interpreting Markdown or changing whitespace.
 * Local files are linked only when explicitly written as Markdown links.
 */
export function splitLinks(text: string): LinkPart[] {
  const pattern = /\b(?:https?:\/\/|www\.)[^\s<>"'`，。！？；：、（）【】「」『』]+/giu;
  const parts: LinkPart[] = [];
  let offset = 0;
  for (const match of text.matchAll(pattern)) {
    const label = trimUrl(match[0]);
    const href = externalUrl(label);
    if (!href) continue;
    if (match.index > offset) parts.push({ text: text.slice(offset, match.index) });
    parts.push({ text: label, href });
    offset = match.index + label.length;
  }
  if (offset < text.length) parts.push({ text: text.slice(offset) });
  return parts;
}
