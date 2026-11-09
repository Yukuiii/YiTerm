import type { ReactNode } from 'react';

function Icon({
  size = 16,
  strokeWidth = 1.7,
  children,
}: {
  size?: number;
  strokeWidth?: number;
  children: ReactNode;
}) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={strokeWidth}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

export const PlusIcon = ({ size = 16 }: { size?: number }) => (
  <Icon size={size}>
    <path d="M12 5v14M5 12h14" />
  </Icon>
);

export const SearchIcon = ({ size = 16 }: { size?: number }) => (
  <Icon size={size}>
    <circle cx="11" cy="11" r="7" />
    <path d="m20 20-3.6-3.6" />
  </Icon>
);

export const FolderIcon = ({ size = 16 }: { size?: number }) => (
  <Icon size={size}>
    <path d="M3.5 7.5a2 2 0 0 1 2-2h3.6l2 2h7.4a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z" />
  </Icon>
);

export const LaptopIcon = ({ size = 16 }: { size?: number }) => (
  <Icon size={size}>
    <rect x="4" y="5" width="16" height="11" rx="1.6" />
    <path d="M2.5 19h19" />
  </Icon>
);

export const SlidersIcon = ({ size = 16 }: { size?: number }) => (
  <Icon size={size}>
    <path d="M4 7h9M17 7h3" />
    <circle cx="15" cy="7" r="2" />
    <path d="M4 12h3M11 12h9" />
    <circle cx="9" cy="12" r="2" />
    <path d="M4 17h9M17 17h3" />
    <circle cx="15" cy="17" r="2" />
  </Icon>
);

export const MoreIcon = ({ size = 16 }: { size?: number }) => (
  <Icon size={size} strokeWidth={0}>
    <circle cx="5" cy="12" r="1.7" fill="currentColor" />
    <circle cx="12" cy="12" r="1.7" fill="currentColor" />
    <circle cx="19" cy="12" r="1.7" fill="currentColor" />
  </Icon>
);

export const ChevronIcon = ({ size = 16, className }: { size?: number; className?: string }) => (
  <svg
    width={size}
    height={size}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth={1.7}
    strokeLinecap="round"
    strokeLinejoin="round"
    className={className}
    aria-hidden="true"
  >
    <path d="m9 6 6 6-6 6" />
  </svg>
);

export const CheckIcon = ({ size = 16, className }: { size?: number; className?: string }) => (
  <svg
    width={size}
    height={size}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth={1.9}
    strokeLinecap="round"
    strokeLinejoin="round"
    className={className}
    aria-hidden="true"
  >
    <path d="m5 12.5 4.4 4.4L19 7" />
  </svg>
);

export const InfoIcon = ({ size = 16 }: { size?: number }) => (
  <Icon size={size}>
    <circle cx="12" cy="12" r="9" />
    <path d="M12 11v6M12 7h.01" />
  </Icon>
);

export const TerminalIcon = ({ size = 16 }: { size?: number }) => (
  <Icon size={size}>
    <path d="m5 7 5 5-5 5" />
    <path d="M13 17h6" />
  </Icon>
);

export const WrenchIcon = ({ size = 16 }: { size?: number }) => (
  <Icon size={size}>
    <path d="M14.7 6.3a5 5 0 0 0-6.4 6.4l-5.7 5.7a2.1 2.1 0 0 0 3 3l5.7-5.7a5 5 0 0 0 6.4-6.4l-3.2 3.2-3-3z" />
  </Icon>
);

export const CodeIcon = ({ size = 16 }: { size?: number }) => (
  <Icon size={size}>
    <path d="m8 6-5 6 5 6" />
    <path d="m16 6 5 6-5 6" />
  </Icon>
);

export const GitBranchIcon = ({ size = 16 }: { size?: number }) => (
  <Icon size={size}>
    <circle cx="6" cy="5" r="2" />
    <circle cx="6" cy="19" r="2" />
    <circle cx="18" cy="5" r="2" />
    <path d="M6 7v10M18 7v3a5 5 0 0 1-5 5H6" />
  </Icon>
);

export const ChatIcon = ({ size = 16 }: { size?: number }) => (
  <Icon size={size}>
    <path d="M20.5 11.5a7.5 7.5 0 0 1-7.5 7.5H8l-4.5 3v-4.4a7.5 7.5 0 0 1 4-10.9 7.5 7.5 0 0 1 13 4.8z" />
  </Icon>
);

export const GearIcon = ({ size = 16 }: { size?: number }) => (
  <Icon size={size}>
    <circle cx="12" cy="12" r="3" />
    <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.9.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.9 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.9l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.9.3h.1A1.7 1.7 0 0 0 10 4.6V4a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.9-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.9v.1a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
  </Icon>
);

export const ArrowUpIcon = ({ size = 16 }: { size?: number }) => (
  <Icon size={size}>
    <path d="M12 19V5" />
    <path d="m5 12 7-7 7 7" />
  </Icon>
);

export const ArrowDownIcon = ({ size = 16 }: { size?: number }) => (
  <Icon size={size}>
    <path d="M12 5v14" />
    <path d="m5 12 7 7 7-7" />
  </Icon>
);

export const SpinnerIcon = ({ size = 16 }: { size?: number }) => (
  <svg
    width={size}
    height={size}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth={2}
    strokeLinecap="round"
    aria-hidden="true"
  >
    <path d="M21 12a9 9 0 1 0-9 9" />
  </svg>
);

export const PaperclipIcon = ({ size = 16 }: { size?: number }) => (
  <Icon size={size}>
    <path d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l8.57-8.57A4 4 0 1 1 18 8.84l-8.59 8.57a2 2 0 0 1-2.83-2.83l8.49-8.48" />
  </Icon>
);

export const FileIcon = ({ size = 16 }: { size?: number }) => (
  <Icon size={size}>
    <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
    <path d="M14 3v5h5" />
  </Icon>
);

export const XIcon = ({ size = 16 }: { size?: number }) => (
  <Icon size={size}>
    <path d="m6 6 12 12M18 6 6 18" />
  </Icon>
);

export const SparkleIcon = ({ size = 16 }: { size?: number }) => (
  <Icon size={size}>
    <path d="m12 3 1.9 5.1L19 10l-5.1 1.9L12 17l-1.9-5.1L5 10l5.1-1.9z" />
    <path d="M19 16.5v3M17.5 18h3" />
  </Icon>
);
