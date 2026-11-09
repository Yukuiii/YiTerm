import type { ReactNode } from 'react';
import { FileIcon, FolderIcon } from './Icons';

type Glyph = 'react' | 'badge' | 'markdown' | 'json' | 'config' | 'image' | 'archive' | 'text' | 'shell' | 'style' | 'html';

interface FileType {
  extensions: string;
  name: string;
  color: string;
  glyph: Glyph;
  label?: string;
  ink?: string;
}

const FILE_TYPES: FileType[] = [
  { extensions: 'tsx jsx', name: 'React', color: '#61dafb', glyph: 'react' },
  { extensions: 'ts mts cts', name: 'TypeScript', color: '#4b9ae8', glyph: 'badge', label: 'TS' },
  { extensions: 'js mjs cjs', name: 'JavaScript', color: '#efd65b', glyph: 'badge', label: 'JS', ink: '#242424' },
  { extensions: 'rs', name: 'Rust', color: '#dea584', glyph: 'badge', label: 'R', ink: '#242424' },
  { extensions: 'py pyw pyi', name: 'Python', color: '#70a4ce', glyph: 'badge', label: 'Py' },
  { extensions: 'go', name: 'Go', color: '#67cbd3', glyph: 'badge', label: 'Go', ink: '#242424' },
  { extensions: 'c h', name: 'C', color: '#8caddd', glyph: 'badge', label: 'C' },
  { extensions: 'cpp cc cxx hpp hh hxx', name: 'C++', color: '#7f9ee8', glyph: 'badge', label: 'C+' },
  { extensions: 'cs', name: 'C#', color: '#aa83d8', glyph: 'badge', label: 'C#' },
  { extensions: 'java', name: 'Java', color: '#da9471', glyph: 'badge', label: 'J' },
  { extensions: 'vue', name: 'Vue', color: '#6dbb93', glyph: 'badge', label: 'V' },
  { extensions: 'svelte', name: 'Svelte', color: '#ed8763', glyph: 'badge', label: 'S' },
  { extensions: 'md mdx markdown', name: 'Markdown', color: '#8fb6e9', glyph: 'markdown' },
  { extensions: 'json jsonc json5', name: 'JSON', color: '#d9bd78', glyph: 'json' },
  { extensions: 'toml yaml yml ini cfg conf lock properties', name: 'Configuration', color: '#a0a9bd', glyph: 'config' },
  { extensions: 'css less', name: 'Stylesheet', color: '#9b9aef', glyph: 'style' },
  { extensions: 'scss sass', name: 'Sass stylesheet', color: '#d995ba', glyph: 'style' },
  { extensions: 'html htm xml svg', name: 'Markup', color: '#e59a77', glyph: 'html' },
  { extensions: 'png jpg jpeg gif webp avif ico bmp tiff tif heic', name: 'Image', color: '#b5a0dc', glyph: 'image' },
  { extensions: 'zip gz tgz bz2 xz zst rar 7z tar', name: 'Archive', color: '#c9b185', glyph: 'archive' },
  { extensions: 'sh bash zsh fish ps1 bat cmd', name: 'Shell script', color: '#8cc698', glyph: 'shell' },
  { extensions: 'txt log csv tsv sql', name: 'Text', color: '#a1adb9', glyph: 'text' },
];

const BY_EXTENSION = new Map(
  FILE_TYPES.flatMap((type) => type.extensions.split(' ').map((extension) => [extension, type] as const)),
);
const CONFIG_NAMES = new Set([
  '.gitignore', '.gitattributes', '.gitmodules', '.editorconfig', '.npmrc', '.nvmrc',
  '.env', 'dockerfile', 'makefile', 'cmakelists.txt',
]);

const GLYPHS: Record<Exclude<Glyph, 'badge'>, ReactNode> = {
  react: <>
    <circle cx="12" cy="12" r="1.7" fill="currentColor" stroke="none" />
    <ellipse cx="12" cy="12" rx="10" ry="3.8" />
    <ellipse cx="12" cy="12" rx="10" ry="3.8" transform="rotate(60 12 12)" />
    <ellipse cx="12" cy="12" rx="10" ry="3.8" transform="rotate(120 12 12)" />
  </>,
  markdown: <>
    <path d="M3 18V6l4.5 5L12 6v12M15 13l3 4 3-4M18 6v11" />
  </>,
  json: <>
    <path d="M9 3H7a2 2 0 0 0-2 2v4l-2 3 2 3v4a2 2 0 0 0 2 2h2M15 3h2a2 2 0 0 1 2 2v4l2 3-2 3v4a2 2 0 0 1-2 2h-2" />
  </>,
  config: <>
    <path d="m9 3-2 4-4 2v6l4 2 2 4h6l2-4 4-2V9l-4-2-2-4Z" />
    <circle cx="12" cy="12" r="3.2" />
  </>,
  image: <>
    <rect x="3" y="4" width="18" height="16" rx="2" />
    <circle cx="8" cy="9" r="1.5" />
    <path d="m3 17 5-5 4 4 4-6 5 7" />
  </>,
  archive: <>
    <rect x="5" y="3" width="14" height="18" rx="2" />
    <path d="M12 3v10" strokeDasharray="2 2" />
    <rect x="10" y="14" width="4" height="4" rx="1" />
  </>,
  text: <>
    <path d="M14 3H6v18h12V7ZM14 3v5h4M9 12h6M9 16h6" />
  </>,
  shell: <>
    <rect x="2.5" y="4" width="19" height="16" rx="2" />
    <path d="m6 8 4 4-4 4M13 16h5" />
  </>,
  style: <>
    <path d="m10 3-4 18M18 3l-4 18M3 9h18M2 15h18" strokeWidth="2.2" />
  </>,
  html: <>
    <path d="m7 7-5 5 5 5M17 7l5 5-5 5M14 4l-4 16" />
  </>,
};

export default function FileTypeIcon({ path, size = 16 }: { path: string; size?: number }) {
  if (path.endsWith('/')) return <FolderIcon size={size} />;
  const name = path.split(/[\\/]/).pop()?.toLowerCase() ?? '';
  const extension = name.includes('.') ? name.slice(name.lastIndexOf('.') + 1) : '';
  const type = CONFIG_NAMES.has(name) || name.startsWith('.env.') || name.startsWith('dockerfile.')
    ? BY_EXTENSION.get('toml')
    : BY_EXTENSION.get(extension);
  if (!type) return <FileIcon size={size} />;

  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.6}
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ color: type.color }}
      aria-hidden="true"
      focusable="false"
    >
      <title>{type.name}</title>
      {type.glyph === 'badge' ? <>
        <rect x="2" y="2" width="20" height="20" rx="2.5" fill="currentColor" stroke="none" />
        <text
          x="12"
          y="16.5"
          textAnchor="middle"
          fill={type.ink ?? '#fff'}
          stroke="none"
          fontFamily="system-ui, sans-serif"
          fontSize="11"
          fontWeight="700"
        >{type.label}</text>
      </> : GLYPHS[type.glyph]}
    </svg>
  );
}
