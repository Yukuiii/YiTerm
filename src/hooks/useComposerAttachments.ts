import { useState } from 'react';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import { chatAttachmentImport, chatImageSave, encodeBytesToBase64 } from '../ipc';

export interface PendingImage {
  id: string;
  path: string;
}

export interface PendingFile {
  id: string;
  path: string;
  name: string;
}

/**
 * Attachments for one composer turn: pasted screenshots, plus files chosen
 * from disk. Images are copied into the conversation cache and pasted into the
 * PTY; other files are referenced by path and read by Claude itself.
 */
export function useComposerAttachments(convId: string, onError: (message: string) => void) {
  const [images, setImages] = useState<PendingImage[]>([]);
  const [files, setFiles] = useState<PendingFile[]>([]);
  /** How many attachments are still being persisted; sending waits for 0. */
  const [attaching, setAttaching] = useState(0);

  /** Persists clipboard images and adds a removable chip for each. */
  const addPastedImages = async (picked: File[]) => {
    setAttaching((n) => n + picked.length);
    for (const file of picked) {
      try {
        const bytes = new Uint8Array(await file.arrayBuffer());
        const path = await chatImageSave(convId, file.type, encodeBytesToBase64(bytes));
        setImages((prev) => [...prev, { id: crypto.randomUUID(), path }]);
      } catch (e) {
        onError(`图片添加失败：${String(e)}`);
      } finally {
        setAttaching((n) => n - 1);
      }
    }
  };

  /** Files chosen from disk: images join the screenshot flow, other files are
   *  referenced by path and read by Claude. */
  const pickAttachments = async () => {
    let picked: string | string[] | null = null;
    try {
      picked = await openDialog({ multiple: true, title: '选择附件' });
    } catch (e) {
      onError(`选择附件失败：${String(e)}`);
      return;
    }
    const paths = picked === null ? [] : Array.isArray(picked) ? picked : [picked];
    if (paths.length === 0) return;
    setAttaching((n) => n + paths.length);
    for (const path of paths) {
      try {
        const att = await chatAttachmentImport(convId, path);
        if (att.kind === 'image') {
          setImages((prev) => [...prev, { id: crypto.randomUUID(), path: att.path }]);
        } else {
          setFiles((prev) => [
            ...prev,
            { id: crypto.randomUUID(), path: att.path, name: att.name },
          ]);
        }
      } catch (e) {
        onError(`附件添加失败：${String(e)}`);
      } finally {
        setAttaching((n) => n - 1);
      }
    }
  };

  const onPaste = (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
    // WebKit exposes pasteboard images through `files` (it converts TIFF to
    // PNG); `items` is the fallback. An empty type still goes to the backend,
    // which sniffs the real content.
    const files: File[] = Array.from(e.clipboardData.files).filter(
      (f) => f.type === '' || f.type.startsWith('image/'),
    );
    if (files.length === 0) {
      for (const item of Array.from(e.clipboardData.items)) {
        if (item.kind === 'file' && item.type.startsWith('image/')) {
          const f = item.getAsFile();
          if (f) files.push(f);
        }
      }
    }
    if (files.length === 0) return; // plain-text paste: leave it to the textarea
    e.preventDefault();
    void addPastedImages(files);
  };

  return {
    images,
    files,
    attaching,
    addPastedImages,
    pickAttachments,
    onPaste,
    /** The turn owns its attachments from the moment it is recorded. */
    clear: () => {
      setImages([]);
      setFiles([]);
    },
    removeImage: (id: string) => setImages((prev) => prev.filter((x) => x.id !== id)),
    removeFile: (id: string) => setFiles((prev) => prev.filter((x) => x.id !== id)),
  };
}
