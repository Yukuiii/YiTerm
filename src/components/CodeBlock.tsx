import type { ReactNode } from 'react';
import CopyButton from './CopyButton';

/**
 * Fenced code block chrome: language label + copy action above the code.
 * `children` is the rendered `<code>` element so URL linkification inside the
 * block keeps working; `code` is the raw source the copy button writes.
 */
export default function CodeBlock({
  language,
  code,
  children,
}: {
  language?: string;
  code: string;
  children: ReactNode;
}) {
  return (
    <div className="md-code-block">
      <div className="md-code-head">
        <span className="md-code-lang">{language?.toLowerCase() || '代码'}</span>
        <CopyButton text={code} className="md-code-copy" label="复制代码" copiedLabel="已复制" />
      </div>
      <pre className="md-code-pre">{children}</pre>
    </div>
  );
}
