import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { openUrl } from '@tauri-apps/plugin-opener';

/**
 * Renders assistant prose as GitHub-flavoured Markdown. Links open in the
 * system browser (never navigate the webview itself).
 */
export default function Markdown({ children }: { children: string }) {
  return (
    <div className="md">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          a: ({ href, children: c }) => (
            <a
              href={href}
              title={href}
              onClick={(e) => {
                e.preventDefault();
                if (href) void openUrl(href).catch(() => {});
              }}
            >
              {c}
            </a>
          ),
          // Wrap tables so wide ones scroll horizontally instead of stretching.
          table: ({ children: c }) => (
            <div className="md-table-wrap">
              <table>{c}</table>
            </div>
          ),
        }}
      >
        {children}
      </ReactMarkdown>
    </div>
  );
}
