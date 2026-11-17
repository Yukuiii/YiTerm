import { invoke } from '@tauri-apps/api/core';

export interface DuckaiCredential {
  /** Ready-to-send `X-Vqd-Hash-1` value. */
  vqd: string;
  /** The webview UA; the challenge and the chat request must agree on it. */
  userAgent: string;
}

interface SolvedChallenge {
  result: unknown;
  stack: string;
  durationMs: number;
}

const TTL_MS = 10 * 60_000;
let cached: { value: DuckaiCredential; at: number } | null = null;

/**
 * Returns a solved Duck.ai challenge. The raw challenge comes from Rust (the
 * frontend cannot read its response header cross-origin); the challenge JS
 * runs here because it measures real DOM layout.
 */
export async function duckaiCredential(): Promise<DuckaiCredential> {
  if (cached && Date.now() - cached.at < TTL_MS) {
    return cached.value;
  }
  const userAgent = navigator.userAgent;
  const challenge = await invoke<string>('duckai_challenge', { userAgent });
  const solved = await solve(challenge);
  const vqd = await invoke<string>('duckai_build_vqd', {
    result: solved.result,
    stack: solved.stack,
    durationMs: solved.durationMs,
  });
  cached = { value: { vqd, userAgent }, at: Date.now() };
  return cached.value;
}

/** Drops the cached VQD after the upstream rejects it. */
export function invalidateDuckaiCredential(): void {
  cached = null;
}

/**
 * Runs the challenge in a sandboxed `data:` iframe. The iframe is a unique
 * opaque origin, so the remote code cannot reach the app's Tauri IPC (which
 * is injected into the main frame only). `allow-same-origin` is required for
 * the challenge's own nested iframe; the data: document stays opaque.
 */
function solve(challenge: string): Promise<SolvedChallenge> {
  return new Promise((resolve, reject) => {
    const started = performance.now();
    const id = `duckai-${crypto.randomUUID()}`;
    const iframe = document.createElement('iframe');
    iframe.setAttribute('sandbox', 'allow-scripts allow-same-origin');
    iframe.setAttribute('aria-hidden', 'true');
    iframe.style.display = 'none';

    let timer: number | undefined;
    const cleanup = () => {
      window.removeEventListener('message', onMessage);
      if (timer !== undefined) window.clearTimeout(timer);
      iframe.remove();
    };
    const onMessage = (event: MessageEvent<unknown>) => {
      if (event.source !== iframe.contentWindow || !isSolverMessage(event.data)) return;
      if (event.data.id !== id) return;
      const { result, error } = event.data;
      cleanup();
      if (error !== undefined) {
        reject(new Error(`Duck.ai 挑战失败：${error}`));
        return;
      }
      if (!isChallengeResult(result)) {
        reject(new Error('Duck.ai 挑战返回格式异常'));
        return;
      }
      resolve({
        result,
        stack: new Error().stack ?? '',
        durationMs: Math.round(performance.now() - started),
      });
    };
    timer = window.setTimeout(() => {
      cleanup();
      reject(new Error('Duck.ai 挑战超时'));
    }, 15_000);
    window.addEventListener('message', onMessage);

    let code: string;
    try {
      code = atob(challenge);
    } catch {
      cleanup();
      reject(new Error('Duck.ai 挑战解码失败'));
      return;
    }
    const html = `<!doctype html><meta charset="utf-8"><body><script>
(async () => {
  try {
    const result = await (${code.replaceAll('</script', '<\\/script')});
    parent.postMessage({ id: ${JSON.stringify(id)}, result }, '*');
  } catch (error) {
    parent.postMessage({ id: ${JSON.stringify(id)}, error: String(error && error.stack || error) }, '*');
  }
})();
</script></body>`;
    iframe.src = `data:text/html;charset=utf-8,${encodeURIComponent(html)}`;
    document.body.appendChild(iframe);
  });
}

function isSolverMessage(
  value: unknown,
): value is { id: string; result?: unknown; error?: string } {
  return typeof value === 'object' && value !== null && 'id' in value && typeof value.id === 'string';
}

function isChallengeResult(value: unknown): value is { client_hashes: unknown[] } {
  return (
    typeof value === 'object' &&
    value !== null &&
    'client_hashes' in value &&
    Array.isArray(value.client_hashes)
  );
}
