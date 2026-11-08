import { useEffect, useRef, useState } from 'react';
import { CheckIcon, ChevronIcon } from './Icons';

export interface DropdownOption<T extends string> {
  value: T;
  label: string;
}

/**
 * Settings dropdown in the app's own style: the native select popup cannot be
 * themed inside WKWebView, so this replaces it with a small popover list.
 */
export default function Dropdown<T extends string>({
  value,
  options,
  onChange,
  disabled = false,
}: {
  value: T;
  options: DropdownOption<T>[];
  onChange: (value: T) => void;
  disabled?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    document.addEventListener('mousedown', onPointerDown);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('mousedown', onPointerDown);
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [open]);

  const selected = options.find((o) => o.value === value);

  return (
    <div className="dropdown" ref={rootRef}>
      <button
        type="button"
        className={`dropdown-btn ${open ? 'open' : ''}`}
        aria-haspopup="listbox"
        aria-expanded={open}
        disabled={disabled}
        onClick={() => setOpen((v) => !v)}
      >
        <span className="dropdown-label">{selected?.label ?? value}</span>
        <ChevronIcon size={13} className={`dropdown-chevron ${open ? 'open' : ''}`} />
      </button>
      {open && (
        <div className="dropdown-menu" role="listbox">
          {options.map((o) => (
            <button
              key={o.value}
              type="button"
              role="option"
              aria-selected={o.value === value}
              className={`dropdown-item ${o.value === value ? 'active' : ''}`}
              onClick={() => {
                onChange(o.value);
                setOpen(false);
              }}
            >
              <span className="dropdown-item-label">{o.label}</span>
              {o.value === value && <CheckIcon size={13} />}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
