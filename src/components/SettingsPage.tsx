import { useState } from 'react';
import { translate, type Settings } from '../ipc';
import { useSettingsStore } from '../stores/settings';

export default function SettingsPage({ onClose }: { onClose: () => void }) {
  const { settings, hasApiKey, save, setApiKey } = useSettingsStore();
  // Edits live in a draft; when untouched the loaded settings are the value.
  const [draft, setDraft] = useState<Settings | null>(null);
  const [apiKeyDraft, setApiKeyDraft] = useState('');
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  const current = draft ?? settings;
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
