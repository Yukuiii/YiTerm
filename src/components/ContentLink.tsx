import { createContext, useContext, useState, type MouseEvent, type ReactNode } from 'react';
import { homeDir, resolve } from '@tauri-apps/api/path';
import { openPath, openUrl } from '@tauri-apps/plugin-opener';
import { externalUrl, fileReference, splitLinks } from '../links';

const InsideLink = createContext(false);
const ProjectFolder = createContext<string | undefined>(undefined);

export function ContentLinkProvider({ folder, children }: { folder?: string; children: ReactNode }) {
  return <ProjectFolder.Provider value={folder}>{children}</ProjectFolder.Provider>;
}

async function openFile(path: string, folder?: string) {
  if (path.startsWith('/')) return openPath(path);
  const root = path.startsWith('~/') ? await homeDir() : folder;
  if (!root) throw new Error('无法确定文件所属的项目目录');
  return openPath(await resolve(root, path.startsWith('~/') ? path.slice(2) : path));
}

/** Open URLs and file references through the desktop's native handlers. */
export default function ContentLink({ href, title, children }: {
  href?: string;
  title?: string;
  children: ReactNode;
}) {
  const folder = useContext(ProjectFolder);
  const [failure, setFailure] = useState<{ target: string; message: string } | null>(null);
  const url = externalUrl(href);
  const file = url ? null : fileReference(href);
  const target = url ?? `${folder ?? ''}:${file?.path ?? ''}`;
  const content = <InsideLink.Provider value={true}>{children}</InsideLink.Provider>;
  if (href?.startsWith('#')) return <a href={href} title={title}>{content}</a>;
  if (!url && (!file || (!file.path.startsWith('/') && !file.path.startsWith('~/') && !folder))) return <>{content}</>;

  const open = async (event: MouseEvent<HTMLAnchorElement>) => {
    event.preventDefault();
    setFailure(null);
    try {
      if (url) await openUrl(url);
      else if (file) await openFile(file.path, folder);
    } catch (error: unknown) {
      setFailure({ target, message: String(error) });
    }
  };
  return (
    <>
      <a
        className="content-link"
        href={url ?? href}
        title={title ?? url ?? file?.path}
        data-file-path={file?.path}
        data-file-line={file?.line}
        target="_blank"
        rel="noopener noreferrer"
        onClick={(event) => void open(event)}
        onAuxClick={(event) => { if (event.button === 1) void open(event); }}
      >
        {content}
      </a>
      {failure?.target === target && (
        <span className="content-link-error" role="alert" title={failure.message}>（打开失败，请重试）</span>
      )}
    </>
  );
}

/** Add links to literal text while preserving its original formatting. */
export function LinkedText({ children }: { children: string }) {
  const insideLink = useContext(InsideLink);
  if (insideLink) return <>{children}</>;
  return <>{splitLinks(children).map((part, index) => part.href ? (
    <ContentLink key={`${index}:${part.href}`} href={part.href}>{part.text}</ContentLink>
  ) : part.text)}</>;
}
