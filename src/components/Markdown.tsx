import { createContext, memo, useContext } from 'react';
import ReactMarkdown, { defaultUrlTransform, type Components } from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { fileReference, remarkContentLinks } from '../links';
import ContentLink, { LinkedText } from './ContentLink';

const MarkdownSource = createContext('');
const components: Components = {
  a: function MarkdownLink({ href, title, children, node }) {
    const source = useContext(MarkdownSource);
    const start = node?.position?.start.offset;
    const end = node?.position?.end.offset;
    const raw = start !== undefined && end !== undefined ? source.slice(start, end) : '';
    // GFM can include Chinese sentence punctuation in a bare URL. Use the same
    // literal-text parser as the rest of chat, leaving explicit link targets intact.
    if (/^(https?:\/\/|www\.)/i.test(raw) && typeof children === 'string') {
      return <LinkedText>{children}</LinkedText>;
    }
    return <ContentLink href={href} title={title}>{children}</ContentLink>;
  },
  code: ({ className, children }) => (
    <code className={className}>
      {typeof children === 'string' ? <LinkedText>{children}</LinkedText> : children}
    </code>
  ),
  // Wrap tables so wide ones scroll horizontally instead of stretching.
  table: ({ children }) => (
    <div className="md-table-wrap">
      <table>{children}</table>
    </div>
  ),
};

/** Renders GFM and opens content URLs in the system browser. */
export default memo(function Markdown({ children }: { children: string }) {
  return (
    <MarkdownSource.Provider value={children}>
      <div className="md">
        <ReactMarkdown
          remarkPlugins={[remarkGfm, remarkContentLinks]}
          components={components}
          urlTransform={(url, key) => key === 'href' && fileReference(url) ? url : defaultUrlTransform(url)}
        >
          {children}
        </ReactMarkdown>
      </div>
    </MarkdownSource.Provider>
  );
});
