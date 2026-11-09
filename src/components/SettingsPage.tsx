import { useCallback, useEffect, useRef, useState } from 'react';
import { getVersion } from '@tauri-apps/api/app';
import {
  apiKeyGet,
  apiKeySet,
  modelDelete,
  modelDownload,
  modelStatus,
  onModelProgress,
  translate,
  type ModelStatus,
  type Settings,
  type TerminalSettings,
  type TranslationSettings,
  type UpdateInfo,
} from '../ipc';
import type { UnlistenFn } from '@tauri-apps/api/event';
import { useSettingsStore } from '../stores/settings';
import Dropdown from './Dropdown';
import { ChatIcon, InfoIcon, SlidersIcon, TerminalIcon, XIcon } from './Icons';

const fmtMB = (bytes: number) => `${(bytes / (1024 * 1024)).toFixed(0)} MB`;

const DEFAULT_TERMINAL: TerminalSettings = { fontFamily: null, fontSize: 13, lineHeight: 1.1 };

const DEEPL_PROTOCOL_OPTIONS: { value: 'deepl' | 'deeplx'; label: string }[] = [
  { value: 'deepl', label: 'DeepL 官方格式' },
  { value: 'deeplx', label: 'DeepLX 网关（text 为字符串）' },
];

type ChatProviderId = 'siliconflow' | 'openrouter' | 'custom';

/** Known OpenAI-compatible endpoints; picking one needs only an API key. */
const CHAT_PROVIDERS: Record<
  Exclude<ChatProviderId, 'custom'>,
  { label: string; baseUrl: string; modelId: string }
> = {
  siliconflow: {
    label: '硅基流动',
    baseUrl: 'https://api.siliconflow.cn/v1',
    modelId: 'Qwen/Qwen3.5-4B',
  },
  openrouter: {
    label: 'OpenRouter',
    baseUrl: 'https://openrouter.ai/api/v1',
    modelId: 'qwen/qwen-2.5-7b-instruct',
  },
};

const CHAT_PROVIDER_OPTIONS: { id: ChatProviderId; label: string }[] = [
  { id: 'siliconflow', label: '硅基流动' },
  { id: 'openrouter', label: 'OpenRouter' },
  { id: 'custom', label: '自定义' },
];

const SETTINGS_NAV = [
  {
    group: '偏好',
    items: [
      { id: 'translation', label: '翻译', Icon: ChatIcon },
      { id: 'terminal', label: '终端', Icon: TerminalIcon },
    ],
  },
  {
    group: '应用',
    items: [
      { id: 'general', label: '通用', Icon: SlidersIcon },
      { id: 'about', label: '关于', Icon: InfoIcon },
    ],
  },
] as const;

type SettingsTab = (typeof SETTINGS_NAV)[number]['items'][number]['id'];

export default function SettingsPage({ onClose, onCheckForUpdates }: {
  onClose: () => void;
  onCheckForUpdates: () => Promise<UpdateInfo | null>;
}) {
  const { settings, save } = useSettingsStore();
  // Edits live in a draft; when untouched the loaded settings are the value.
  const [draft, setDraft] = useState<Settings | null>(null);
  const [tab, setTab] = useState<SettingsTab>('translation');
  const [appVersion, setAppVersion] = useState('读取中…');
  const [checkingUpdates, setCheckingUpdates] = useState(false);
  const [updateResult, setUpdateResult] = useState<{ message: string; error: boolean } | null>(null);
  const updateRequest = useRef(0);
  const [apiKeyDraft, setApiKeyDraft] = useState('');
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [model, setModel] = useState<ModelStatus | null>(null);
  const [hasKey, setHasKey] = useState(false);
  const terminalSaveTimer = useRef<number | undefined>(undefined);

  const current = draft ?? settings;
  // Provider is stored per key, so the "saved" state follows the draft.
  const activeChatProvider: ChatProviderId =
    current?.translation.chatProvider ?? 'custom';
  // DeepL has its own key slot; chat mode keys follow the selected provider.
  const activeKeyProvider =
    current?.translation.mode === 'deepl' ? 'deepl' : activeChatProvider;

  useEffect(() => {
    let alive = true;
    void getVersion()
      .then((version) => {
        if (alive) setAppVersion(version);
      })
      .catch(() => {
        if (alive) setAppVersion('无法读取版本');
      });
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => () => { updateRequest.current++; }, []);

  // Status is best-effort; on failure the section simply stays blank.
  const refreshModel = useCallback(() => {
    modelStatus()
      .then((s) => setModel(s))
      .catch(() => {});
  }, []);

  useEffect(() => {
    let alive = true;
    apiKeyGet(activeKeyProvider)
      .then((key) => {
        if (alive) setHasKey(!!key);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [activeKeyProvider]);

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
  // Typography is tuned by eye: apply terminal changes immediately and persist
  // them without waiting for the bottom 保存 button.
  const patchTranslation = (patch: Partial<TranslationSettings>) =>
    setDraft({ ...current, translation: { ...t, ...patch } });
  const updateTerminal = (patch: Partial<TerminalSettings>) => {
    const terminal = { ...(current.terminal ?? DEFAULT_TERMINAL), ...patch };
    setDraft({ ...current, terminal });
    const stored = useSettingsStore.getState().settings;
    if (!stored) return;
    if (terminalSaveTimer.current !== undefined) {
      window.clearTimeout(terminalSaveTimer.current);
    }
    terminalSaveTimer.current = window.setTimeout(() => {
      void save({ ...stored, terminal });
    }, 200);
  };
  const chatProvider: ChatProviderId = t.chatProvider ?? 'custom';
  const providerPreset = chatProvider === 'custom' ? null : CHAT_PROVIDERS[chatProvider];
  const deeplProtocol = t.deeplProtocol ?? 'deepl';
  // Google and the built-in DeepL channel ("dlx") share the "free channel"
  // choice in the UI.
  const freeChannel: 'google' | 'dlx' | null =
    t.mode === 'google' ? 'google' : t.mode === 'dlx' ? 'dlx' : null;
  const deepLApi = t.mode === 'deepl';

  /** Selecting a known provider pre-fills its endpoint and a default model. */
  const pickChatProvider = (id: ChatProviderId) => {
    const preset = id === 'custom' ? null : CHAT_PROVIDERS[id];
    // A key typed for the previous provider must not be saved under the new one.
    setApiKeyDraft('');
    setDraft({
      ...current,
      translation: {
        ...t,
        chatProvider: id,
        ...(preset ? { chatBaseUrl: preset.baseUrl, chatModelId: preset.modelId } : {}),
      },
    });
  };

  const saveAll = async () => {
    await save(current);
    if (apiKeyDraft.trim()) {
      await apiKeySet(apiKeyDraft.trim(), activeKeyProvider);
      setApiKeyDraft('');
      setHasKey(true);
    }
    setSaved(true);
    window.setTimeout(() => setSaved(false), 1500);
    refreshModel();
  };

  const checkForUpdates = async () => {
    const request = ++updateRequest.current;
    setCheckingUpdates(true);
    setUpdateResult(null);
    try {
      const info = await onCheckForUpdates();
      if (request === updateRequest.current) {
        setUpdateResult({
          message: info ? `发现新版本 ${info.latestVersion}。` : '当前已是最新版本。',
          error: false,
        });
      }
    } catch (e) {
      if (request === updateRequest.current) {
        setUpdateResult({ message: `检查更新失败：${String(e)}`, error: true });
      }
    } finally {
      if (request === updateRequest.current) setCheckingUpdates(false);
    }
  };

  const runTest = async () => {
    setTesting(true);
    setTestResult(null);
    // Apply the draft first so the backend uses it, then test.
    await save(current);
    if (apiKeyDraft.trim()) {
      await apiKeySet(apiKeyDraft.trim(), activeKeyProvider);
      setApiKeyDraft('');
      setHasKey(true);
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
          <div className="settings-head-actions">
            {tab !== 'about' && (
              <button className="primary-btn settings-save" onClick={() => void saveAll()}>
                {saved ? '已保存 ✓' : '保存'}
              </button>
            )}
            <button className="icon-btn" onClick={onClose} aria-label="关闭设置">
              <XIcon size={16} />
            </button>
          </div>
        </div>

        <div className="settings-main">
          <nav className="settings-nav" aria-label="设置分类">
            {SETTINGS_NAV.map((group) => (
              <div className="settings-nav-group" key={group.group}>
                <div className="settings-nav-title">{group.group}</div>
                {group.items.map(({ id, label, Icon }) => (
                  <button
                    key={id}
                    type="button"
                    className={`settings-nav-item ${tab === id ? 'active' : ''}`}
                    aria-current={tab === id ? 'page' : undefined}
                    onClick={() => setTab(id)}
                  >
                    <Icon size={16} />
                    <span>{label}</span>
                  </button>
                ))}
              </div>
            ))}
          </nav>

          <div className="settings-content">
            {tab === 'translation' && (
              <>
                <h2 className="settings-section-title">翻译</h2>
                <section>
          <label className="radio-row">
            <input
              type="radio"
              name="mode"
              checked={freeChannel !== null}
              onChange={() => {
                if (freeChannel === null) {
                  setDraft({ ...current, translation: { ...t, mode: 'google' } });
                }
              }}
            />
            <div>
              <div className="radio-title">免费通道</div>
              <div className="radio-desc">Google 翻译或 DeepL 免费通道，无需 API Key</div>
            </div>
          </label>
          {freeChannel !== null && (
            <div className="chat-config">
              <label>
                服务
                <div className="mode-switch">
                  <button
                    type="button"
                    className={`mode-btn ${freeChannel === 'google' ? 'active' : ''}`}
                    onClick={() => setDraft({ ...current, translation: { ...t, mode: 'google' } })}
                  >
                    Google
                  </button>
                  <button
                    type="button"
                    className={`mode-btn ${freeChannel === 'dlx' ? 'active' : ''}`}
                    onClick={() =>
                      setDraft({
                        ...current,
                        translation: { ...t, mode: 'dlx' },
                      })
                    }
                  >
                    DeepL（DLX）
                  </button>
                </div>
              </label>
              <div className="radio-desc">
                {freeChannel === 'google'
                  ? '使用 Google 免费端点；可能遇到限流'
                  : '内置 DeepL 匿名通道，无需 Key、无需自建服务；有频率限制'}
              </div>
            </div>
          )}
          <label className="radio-row">
            <input
              type="radio"
              name="mode"
              checked={deepLApi}
              onChange={() =>
                setDraft({
                  ...current,
                  translation: { ...t, mode: 'deepl' },
                })
              }
            />
            <div>
              <div className="radio-title">DeepL API / 网关</div>
              <div className="radio-desc">官方 API（免费 Key 每月 50 万字符），或 DeepLX 兼容网关</div>
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
              <div className="radio-desc">硅基流动 / OpenRouter 预设，或任意 OpenAI 兼容接口</div>
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
                供应商
                <div className="mode-switch">
                  {CHAT_PROVIDER_OPTIONS.map((p) => (
                    <button
                      key={p.id}
                      type="button"
                      className={`mode-btn ${chatProvider === p.id ? 'active' : ''}`}
                      onClick={() => pickChatProvider(p.id)}
                    >
                      {p.label}
                    </button>
                  ))}
                </div>
              </label>
              {chatProvider === 'custom' && (
                <label>
                  Base URL
                  <input
                    type="text"
                    placeholder="https://api.deepseek.com/v1"
                    value={t.chatBaseUrl ?? ''}
                    onChange={(e) => setDraft({ ...current, translation: { ...t, chatBaseUrl: e.target.value } })}
                  />
                </label>
              )}
              <label>
                模型 ID
                <input
                  type="text"
                  placeholder={providerPreset?.modelId ?? 'deepseek-chat'}
                  value={t.chatModelId ?? ''}
                  onChange={(e) => setDraft({ ...current, translation: { ...t, chatModelId: e.target.value } })}
                />
              </label>
              <label>
                <span className="field-label">
                  API Key{providerPreset ? `（${providerPreset.label}）` : ''}
                  {hasKey && <span className="key-badge">已保存</span>}
                </span>
                <input
                  type="password"
                  placeholder={hasKey ? '（已保存，输入可覆盖）' : 'sk-…'}
                  value={apiKeyDraft}
                  onChange={(e) => setApiKeyDraft(e.target.value)}
                />
              </label>
              {hasKey && (
                <button
                  className="ghost-btn"
                  onClick={async () => {
                    await apiKeySet(null, activeKeyProvider);
                    setHasKey(false);
                  }}
                >
                  清除已保存的 API Key
                </button>
              )}
            </div>
          )}

          {deepLApi && (
            <div className="chat-config">
              <label>
                协议
                <Dropdown
                  value={deeplProtocol}
                  options={DEEPL_PROTOCOL_OPTIONS}
                  onChange={(v) => patchTranslation({ deeplProtocol: v })}
                />
              </label>
              <div className="radio-desc">
                官方格式自动使用 Authorization: DeepL-Auth-Key；DeepLX 网关自动使用
                Authorization: Bearer（Key 通常已包含在地址中）。
              </div>
              <label>
                <span className="field-label">
                  API Key（DeepLX 把 Key 放在地址里时可留空）
                  {hasKey && <span className="key-badge">已保存</span>}
                </span>
                <input
                  type="password"
                  placeholder={
                    hasKey ? '（已保存，输入可覆盖）' : 'xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx:fx'
                  }
                  value={apiKeyDraft}
                  onChange={(e) => setApiKeyDraft(e.target.value)}
                />
              </label>
              {hasKey && (
                <button
                  className="ghost-btn"
                  onClick={async () => {
                    await apiKeySet(null, activeKeyProvider);
                    setHasKey(false);
                  }}
                >
                  清除已保存的 API Key
                </button>
              )}
              <label>
                API 地址（留空自动判断）
                <input
                  type="text"
                  placeholder="https://api-free.deepl.com/v2/translate"
                  value={t.deeplApiUrl ?? ''}
                  onChange={(e) =>
                    setDraft({
                      ...current,
                      translation: { ...t, deeplApiUrl: e.target.value || null },
                    })
                  }
                />
              </label>
              <div className="radio-desc">
                官方格式：:fx 免费 Key 走 api-free.deepl.com，其余走 api.deepl.com。DeepLX
                网关通常把 Key 放在地址里（如 api.deeplx.org/&lt;key&gt;/translate），请求体
                text 为字符串、返回 code/data。
              </div>
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
              </>
            )}

            {tab === 'terminal' && (
              <>
                <h2 className="settings-section-title">终端</h2>
                <section>
                <div className="setting-rows">
                  <div className="setting-row">
                    <div className="setting-copy">
                      <div className="setting-title">字体</div>
                      <div className="setting-desc">留空使用内置 Ioskeley Mono</div>
                    </div>
                    <input
                      type="text"
                      placeholder="例如 JetBrains Mono"
                      value={(current.terminal ?? DEFAULT_TERMINAL).fontFamily ?? ''}
                      onChange={(e) => updateTerminal({ fontFamily: e.target.value || null })}
                    />
                  </div>
                  <div className="setting-row">
                    <div className="setting-copy">
                      <div className="setting-title">字号</div>
                      <div className="setting-desc">可选范围 8–32</div>
                    </div>
                    <input
                      type="number"
                      min={8}
                      max={32}
                      step={0.5}
                      value={(current.terminal ?? DEFAULT_TERMINAL).fontSize}
                      onChange={(e) => {
                        const n = Number(e.target.value);
                        if (Number.isFinite(n)) updateTerminal({ fontSize: n });
                      }}
                    />
                  </div>
                  <div className="setting-row">
                    <div className="setting-copy">
                      <div className="setting-title">行高</div>
                      <div className="setting-desc">可选范围 0.8–3.0</div>
                    </div>
                    <input
                      type="number"
                      min={0.8}
                      max={3}
                      step={0.1}
                      value={(current.terminal ?? DEFAULT_TERMINAL).lineHeight}
                      onChange={(e) => {
                        const n = Number(e.target.value);
                        if (Number.isFinite(n)) updateTerminal({ lineHeight: n });
                      }}
                    />
                  </div>
                </div>
                </section>
              </>
            )}

            {tab === 'translation' && (
              <section>
                <div className="setting-row">
                  <div className="setting-copy">
                    <div className="setting-title">测试</div>
                    <div className="setting-desc">用当前配置翻译一句“你好，世界！”</div>
                  </div>
                  <button className="primary-btn" onClick={() => void runTest()} disabled={testing}>
                    {testing ? '测试中…' : '测试'}
                  </button>
                </div>
                {testResult && <div className="test-result">{testResult}</div>}
              </section>
            )}

            {tab === 'general' && (
              <>
                <h2 className="settings-section-title">通用</h2>
                <section>
                  <div className="setting-row">
                    <div className="setting-copy">
                      <div className="setting-title">Claude CLI</div>
                      <div className="setting-desc">
                        留空自动检测 ~/.local/bin/claude 或 PATH
                      </div>
                    </div>
                    <input
                      type="text"
                      placeholder="~/.local/bin/claude"
                      value={current.claudePath ?? ''}
                      onChange={(e) => setDraft({ ...current, claudePath: e.target.value || null })}
                    />
                  </div>
                </section>
              </>
            )}
            {tab === 'about' && (
              <>
                <h2 className="settings-section-title">关于</h2>
                <section>
                  <div className="setting-row">
                    <div className="setting-copy">
                      <div className="setting-title">YiTerm</div>
                      <div className="setting-desc">当前应用版本</div>
                    </div>
                    <span className="setting-title">{appVersion}</span>
                  </div>
                  <div className="setting-row">
                    <div className="setting-copy">
                      <div className="setting-title">应用更新</div>
                      <div className="setting-desc">每小时自动检查，也可手动检查新版本。</div>
                    </div>
                    <button
                      className="primary-btn"
                      onClick={() => void checkForUpdates()}
                      disabled={checkingUpdates}
                      aria-busy={checkingUpdates}
                    >
                      {checkingUpdates ? '正在检查…' : 'Check for Updates'}
                    </button>
                  </div>
                  {updateResult && (
                    <div className="test-result" role={updateResult.error ? 'alert' : 'status'}>
                      {updateResult.message}
                    </div>
                  )}
                </section>
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
