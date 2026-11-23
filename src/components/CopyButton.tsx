import { useEffect, useRef, useState } from 'react';

type CopyState = 'idle' | 'copied' | 'failed';

/**
 * Small clipboard action with the reference's transient confirmation state.
 * `text` is copied verbatim; callers style the button for their surface.
 */
export default function CopyButton({
  text,
  label = '复制',
  copiedLabel = '已复制',
  failedLabel = '复制失败',
  className = '',
  title,
}: {
  text: string;
  label?: string;
  copiedLabel?: string;
  failedLabel?: string;
  className?: string;
  title?: string;
}) {
  const [state, setState] = useState<CopyState>('idle');
  const timer = useRef<number | undefined>(undefined);

  useEffect(() => () => {
    if (timer.current !== undefined) window.clearTimeout(timer.current);
  }, []);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setState('copied');
    } catch {
      setState('failed');
    }
    if (timer.current !== undefined) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setState('idle'), 2000);
  };

  const shown = state === 'copied' ? copiedLabel : state === 'failed' ? failedLabel : label;
  return (
    <button
      type="button"
      className={`copy-btn ${state === 'idle' ? '' : `is-${state}`} ${className}`.trim()}
      title={title ?? label}
      aria-label={title ?? label}
      onClick={() => void copy()}
    >
      {shown}
    </button>
  );
}
