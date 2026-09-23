/** Agent 提供商元信息 */

/** 单条可配置字段（沿用 ASR/TTS 的 ProviderField 本地声明模式） */
export interface ProviderField {
  /** 配置 key，对应 settings.toml 中 providers.<id>.key */
  key: string;
  /** 界面显示名 */
  label: string;
  /** 输入控件类型 */
  type: 'password' | 'text' | 'select';
  placeholder?: string;
  options?: string[];
}

export interface ProviderInfo {
  /** 唯一标识（如 "openclaw" "codex"） */
  id: string;
  /** 显示名称 */
  name: string;
  /** 该提供商的可配置字段（如 cli_path） */
  fields: ProviderField[];
}

/** 所有支持的 Agent 提供商 */
export const AGENT_PROVIDERS: ProviderInfo[] = [
  {
    id: 'openclaw',
    name: 'OpenClaw',
    fields: [
      {
        key: 'cli_path',
        label: 'CLI 路径',
        type: 'text',
        placeholder: '留空使用 PATH 查找 openclaw',
      },
    ],
  },
  {
    id: 'codex',
    name: 'Codex CLI',
    fields: [
      {
        key: 'model',
        label: '模型 ID',
        type: 'text',
        placeholder: '留空跟随 Codex 默认，例如 gpt-6-astra',
      },
      {
        key: 'model_reasoning_effort',
        label: '思考强度',
        type: 'select',
        placeholder: '跟随 Codex 默认',
        options: ['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max', 'ultra'],
      },
      {
        key: 'cli_path',
        label: 'CLI 路径',
        type: 'text',
        placeholder: '留空使用 PATH 查找 codex',
      },
    ],
  },
  {
    id: 'ollama',
    name: 'Ollama',
    fields: [
      {
        key: 'model_id',
        label: '模型 ID',
        type: 'text',
        placeholder: '例如 qwen3:8b（先运行 ollama pull）',
      },
      {
        key: 'base_url',
        label: '服务地址',
        type: 'text',
        placeholder: 'http://localhost:11434',
      },
    ],
  },
  {
    id: 'custom',
    name: '自定义 Agent',
    fields: [
      {
        key: 'base_url',
        label: 'Base URL',
        type: 'text',
        placeholder: 'https://api.example.com/v1',
      },
      {
        key: 'model_id',
        label: '模型 ID',
        type: 'text',
        placeholder: '输入服务商提供的模型 ID',
      },
      {
        key: 'api_key',
        label: 'API Key',
        type: 'password',
        placeholder: '输入 API Key（也可使用环境变量引用）',
      },
    ],
  },
];

/** 按 id 查找提供商信息 */
export function getAgentProviderInfo(id: string): ProviderInfo | undefined {
  return AGENT_PROVIDERS.find((p) => p.id === id);
}
