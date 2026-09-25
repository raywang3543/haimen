/** ASR 提供商元信息 */

export interface ProviderField {
  /** 字段键名（存到 providers[providerId][key]） */
  key: string;
  /** 界面显示标签 */
  label: string;
  /** 输入类型 */
  type: 'password' | 'text';
  /** 空值时的占位提示 */
  placeholder?: string;
}

export interface ProviderInfo {
  /** 唯一标识（如 "doubao" "qwen"） */
  id: string;
  /** 显示名称 */
  name: string;
  description?: string;
  /** 配置字段列表 */
  fields: ProviderField[];
}

/** 所有支持的 ASR 服务商 */
export const ASR_PROVIDERS: ProviderInfo[] = [
  {
    id: 'sensevoice',
    name: 'SenseVoice 本地',
    description: '离线识别；录音结束后返回文本，本地音量检测负责判停。',
    fields: [
      {
        key: 'model_dir',
        label: '模型目录',
        type: 'text',
        placeholder: '默认：models/sensevoice-small-v1',
      },
    ],
  },
  {
    id: 'doubao',
    name: '火山引擎',
    fields: [
      {
        key: 'api_key',
        label: 'API Key',
        type: 'password',
        placeholder: '未设置，可用环境变量 DOUBAO_API_KEY',
      },
    ],
  },
];

/** 按 id 查找提供商信息 */
export function getProviderInfo(id: string): ProviderInfo | undefined {
  return ASR_PROVIDERS.find((p) => p.id === id);
}
