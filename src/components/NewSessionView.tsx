import { useState } from 'react';
import { useComposerKeyboard } from '../hooks/useComposerKeyboard';
import Mascot from './Mascot';
import { folderName } from '../format';
import {
  ArrowUpIcon,
  CheckIcon,
  ChevronIcon,
  FolderIcon,
  LaptopIcon,
  PlusIcon,
  SparkleIcon,
  TerminalIcon,
} from './Icons';

interface NewSessionViewProps {
  folders: string[];
  selectedFolder: string | null;
  onSelectFolder: (folder: string) => void;
  onPickFolder: () => void;
  onStartEmpty: () => void | Promise<void>;
  onOpenSettings: () => void;
  onPrompt: (text: string) => Promise<void>;
}

export default function NewSessionView({
  folders,
  selectedFolder,
  onSelectFolder,
  onPickFolder,
  onStartEmpty,
  onOpenSettings,
  onPrompt,
}: NewSessionViewProps) {
  const [text, setText] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async () => {
    const prompt = text.trim();
    if (!prompt || busy) return;
    if (!selectedFolder) {
      onPickFolder();
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await onPrompt(prompt);
      setText('');
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const composerKeyboard = useComposerKeyboard(submit);

  return (
    <div className="new-session">
      <div className="new-stage">
        <div className="new-inner">
          <Mascot />

          <div className="float-card">
            {folders.slice(0, 4).map((folder) => (
              <button
                key={folder}
                className="card-row"
                title={folder}
                onClick={() => onSelectFolder(folder)}
              >
                <LaptopIcon size={17} />
                <span className="grow">{folderName(folder)}</span>
                {folder === selectedFolder && <CheckIcon size={16} className="check" />}
              </button>
            ))}

            {selectedFolder && !folders.includes(selectedFolder) && (
              <button className="card-row" title={selectedFolder} onClick={() => onSelectFolder(selectedFolder)}>
                <LaptopIcon size={17} />
                <span className="grow">{folderName(selectedFolder)}</span>
                <CheckIcon size={16} className="check" />
              </button>
            )}

            {folders.length > 0 && <div className="card-divider" />}

            <button className="card-row" onClick={onPickFolder}>
              <FolderIcon size={17} />
              <span className="grow">选择其他文件夹…</span>
              <ChevronIcon size={15} className="chev" />
            </button>
          </div>

          <div className="float-card">
            <button className="card-row" onClick={() => void onStartEmpty()} disabled={!selectedFolder}>
              <TerminalIcon size={17} />
              <span className="grow">直接打开空会话</span>
            </button>
            <button className="card-row" onClick={onOpenSettings}>
              <SparkleIcon size={17} />
              <span className="grow">打开翻译设置</span>
            </button>
          </div>
        </div>
      </div>

      {error && <div className="new-error">{error}</div>}

      <div className="new-composer">
        <div className="composer">
          <button className="composer-icon-btn" title="选择项目文件夹" onClick={onPickFolder}>
            <PlusIcon size={18} />
          </button>
          <textarea
            value={text}
            rows={1}
            placeholder={selectedFolder ? '描述任务或提问…' : '先选择一个项目文件夹…'}
            onChange={(e) => setText(e.target.value)}
            {...composerKeyboard}
          />
          <button
            className="send-btn"
            title="开始会话"
            onClick={() => void submit()}
            disabled={!text.trim() || busy}
          >
            <ArrowUpIcon size={16} />
          </button>
        </div>
      </div>
    </div>
  );
}
