import { useCallback, useEffect, useState } from 'react';
import {
  modelDelete,
  modelDownload,
  modelStatus,
  onModelProgress,
  translate,
  type ModelStatus,
  type Settings,
} from '../ipc';
import type { UnlistenFn } from '@tauri-apps/api/event';
import { useSettingsStore } from '../stores/settings';

const fmtMB = (bytes: number) => `${(bytes / (1024 * 1024)).toFixed(0)} MB`;

export default function SettingsPage({ onClose }: { onClose: () => void }) {
  const { settings, hasApiKey, save, setApiKey } = useSettingsStore();
  // Edits live in a draft; when untouched the loaded settings are the value.
  const [draft, setDraft] = useState<Settings | null>(null);
  const [apiKeyDraft, setApiKeyDraft] = useState('');
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [model, setModel] = useState<ModelStatus | null>(null);

  const current = draft ?? settings;

  // Status is best-effort; on failure the section simply stays blank.
  const refreshModel = useCallback(() => {
    modelStatus()
      .then((s) => setModel(s))
      .catch(() => {});
  }, []);

  useEffect(() => {
    refreshModel();
    let unlisten: UnlistenFn | undefined;
    let alive = true;
    void onModelProgress((p) => {
      setModel((prev) => ({
        state: p.state,
        downloaded: p.downloaded,
        total: p.total,
        path: prev?.path ?? '',
        message: p.message,
      }));
      if (p.state === 'ready') refreshModel();
    }).then((un) => {
      if (alive) unlisten = un;
      else un();
    });
    return () => {
      alive = false;
      unlisten?.();
    };
  }, [refreshModel]);

  if (!current) return null;
  const t = current.translation;

  const saveAll = async () => {
    await save(current);
    if (apiKeyDraft.trim()) {
      await setApiKey(apiKeyDraft.trim());
      setApiKeyDraft('');
    }
    setSaved(true);
    window.setTimeout(() => setSaved(false), 1500);
    refreshModel();
  };

  const runTest = async () => {
    setTesting(true);
    setTestResult(null);
    // Apply the draft first so the backend uses it, then test.
    await save(current);
    if (apiKeyDraft.trim()) {
      await setApiKey(apiKeyDraft.trim());
      setApiKeyDraft('');
    }
    try {
      const out = await translate('zh-en', '你好，世界！', () => {});
      setTestResult(`✅ "${out}"`);
    } catch (e) {
      setTestResult(`❌ ${String(e)}`);
    } finally {
      setTesting(false);
    }
  };

  const startDownload = async () => {
    try {
      await modelDownload();
    } catch (e) {
      setTestResult(`❌ ${String(e)}`);
    }
  };

  const removeModel = async () => {
    if (!window.confirm('删除本地翻译模型（约 907 MB）？下次使用需重新下载。')) return;
    try {
      await modelDelete();
    } finally {
      refreshModel();
    }
  };

  const pct =
    model && model.total > 0
      ? Math.min(100, Math.floor((model.downloaded / model.total) * 100))
      : 0;

  return (
    <div className="settings-overlay">
      <div className="settings-card">
        <div className="settings-head">
          <h2>设置</h2>
          <button className="icon-btn" onClick={onClose}>✕</button>
        </div>

        <section>
          <h3>翻译方式</h3>
          <label className="radio-row">
            <input
              type="radio"
              name="mode"
              checked={t.mode === 'google'}
              onChange={() => setDraft({ ...current, translation: { ...t, mode: 'google' } })}
            />
            <div>
              <div className="radio-title">Google 翻译（内置）</div>
              <div className="radio-desc">免 Key，使用 Google 免费端点；可能遇到限流</div>
            </div>
          </label>
          <label className="radio-row">
            <input
              type="radio"
              name="mode"
              checked={t.mode === 'chat'}
              onChange={() => setDraft({ ...current, translation: { ...t, mode: 'chat' } })}
            />
            <div>
              <div className="radio-title">Chat 模式</div>
              <div className="radio-desc">任意 OpenAI 兼容接口（OpenAI / DeepSeek / Qwen / 自建…）</div>
            </div>
          </label>
          <label className="radio-row">
            <input
              type="radio"
              name="mode"
              checked={t.mode === 'local'}
              onChange={() => setDraft({ ...current, translation: { ...t, mode: 'local' } })}
            />
            <div>
              <div className="radio-title">本地模型 Hy-MT2-1.8B</div>
              <div className="radio-desc">离线翻译，不依赖网络；占用约 907 MB 磁盘，运行时约占 2–3 GB 内存</div>
            </div>
          </label>

          {t.mode === 'chat' && (
            <div className="chat-config">
              <label>
                Base URL
                <input
                  type="text"
                  placeholder="https://api.deepseek.com/v1"
                  value={t.chatBaseUrl ?? ''}
                  onChange={(e) => setDraft({ ...current, translation: { ...t, chatBaseUrl: e.target.value } })}
                />
              </label>
              <label>
                模型 ID
                <input
                  type="text"
                  placeholder="deepseek-chat"
                  value={t.chatModelId ?? ''}
                  onChange={(e) => setDraft({ ...current, translation: { ...t, chatModelId: e.target.value } })}
                />
              </label>
              <label>
                API Key {hasApiKey && <span className="key-badge">已存于钥匙串</span>}
                <input
                  type="password"
                  placeholder={hasApiKey ? '（已保存，输入可覆盖）' : 'sk-…'}
                  value={apiKeyDraft}
                  onChange={(e) => setApiKeyDraft(e.target.value)}
                />
              </label>
              {hasApiKey && (
                <button
                  className="ghost-btn"
                  onClick={async () => { await setApiKey(null); }}
                >
                  清除已保存的 API Key
                </button>
              )}
            </div>
          )}

          {t.mode === 'local' && (
            <div className="chat-config">
              <div className="model-status">
                {!model && <span>正在读取模型状态…</span>}
                {model?.state === 'ready' && (
                  <span className="model-ready">✓ 模型已就绪（{model.path}）</span>
                )}
                {model?.state === 'missing' && (
                  <span>模型未下载（约 907 MB），点击下方「下载模型」开始下载。</span>
                )}
                {model?.state === 'downloading' && (
                  <div className="model-progress-wrap">
                    <div className="model-progress">
                      <div className="model-progress-fill" style={{ width: `${pct}%` }} />
                    </div>
                    <span>
                      下载中 {pct}%（{fmtMB(model.downloaded)} / {fmtMB(model.total)}）
                    </span>
                  </div>
                )}
                {model?.state === 'error' && (
                  <span className="model-error">
                    ❌ 下载失败：{model.message ?? '未知错误'}
                  </span>
                )}
              </div>
              <div className="model-actions">
                {(model?.state === 'missing' || model?.state === 'error') && (
                  <button className="primary-btn" onClick={() => void startDownload()}>
                    {model?.state === 'error' ? '重试下载' : '下载模型'}
                  </button>
                )}
                {model?.state === 'ready' && (
                  <button className="ghost-btn" onClick={() => void removeModel()}>
                    删除模型文件
                  </button>
                )}
              </div>
            </div>
          )}
        </section>

        <section>
          <h3>测试</h3>
          <button className="primary-btn" onClick={() => void runTest()} disabled={testing}>
            {testing ? '测试中…' : '翻译一句试试：“你好，世界！”'}
          </button>
          {testResult && <div className="test-result">{testResult}</div>}
        </section>

        <section>
          <h3>Claude CLI</h3>
          <label>
            claude 可执行文件路径（留空自动检测）
            <input
              type="text"
              placeholder="~/.local/bin/claude"
              value={current.claudePath ?? ''}
              onChange={(e) => setDraft({ ...current, claudePath: e.target.value || null })}
            />
          </label>
        </section>

        <div className="settings-foot">
          <button className="primary-btn" onClick={() => void saveAll()}>
            {saved ? '已保存 ✓' : '保存'}
          </button>
        </div>
      </div>
    </div>
  );
}
