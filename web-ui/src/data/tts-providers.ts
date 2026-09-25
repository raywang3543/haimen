/** TTS 提供商元信息 */

export interface ProviderField {
  /** 字段键名（存到 providers[providerId][key]） */
  key: string;
  /** 界面显示标签 */
  label: string;
  /** 输入类型 */
  type: 'password' | 'text' | 'select' | 'number' | 'voice';
  /** 空值时的占位提示 */
  placeholder?: string;
  /** select 类型的选项列表 */
  options?: string[];
  /** 未保存配置时展示并使用的默认值 */
  defaultValue?: string;
  min?: number;
  max?: number;
  step?: number;
}

export interface ProviderInfo {
  /** 唯一标识（如 "doubao" "qwen"） */
  id: string;
  /** 显示名称 */
  name: string;
  /** 配置字段列表 */
  fields: ProviderField[];
}

/** 所有支持的 TTS 服务商 */
export const TTS_PROVIDERS: ProviderInfo[] = [
  {
    id: 'macos_native',
    name: 'macOS 原生 TTS',
    fields: [
      {
        key: 'voice',
        label: '音色',
        type: 'voice',
        defaultValue: 'com.apple.voice.premium.zh-CN.Yue',
      },
      {
        key: 'rate',
        label: '语速（0–1）',
        type: 'number',
        defaultValue: '0.5',
        min: 0,
        max: 1,
        step: 0.01,
      },
      {
        key: 'pitch',
        label: '音调倍率（0.5–2）',
        type: 'number',
        defaultValue: '1.0',
        min: 0.5,
        max: 2,
        step: 0.01,
      },
      {
        key: 'volume',
        label: '音量（0–1）',
        type: 'number',
        defaultValue: '1.0',
        min: 0,
        max: 1,
        step: 0.01,
      },
      {
        key: 'pre_delay',
        label: '朗读前停顿（秒）',
        type: 'number',
        defaultValue: '0',
        min: 0,
        step: 0.1,
      },
      {
        key: 'post_delay',
        label: '朗读后停顿（秒）',
        type: 'number',
        defaultValue: '0',
        min: 0,
        step: 0.1,
      },
    ],
  },
  {
    id: 'edge_tts',
    name: '本地 Edge TTS',
    fields: [
      { key: 'voice', label: '音色', type: 'text', placeholder: 'zh-CN-XiaoxiaoNeural' },
      { key: 'rate', label: '语速', type: 'text', placeholder: '+0%' },
      { key: 'volume', label: '音量', type: 'text', placeholder: '+0%' },
      { key: 'pitch', label: '音调', type: 'text', placeholder: '+0Hz' },
      { key: 'proxy', label: '代理（可选）', type: 'text', placeholder: 'http://127.0.0.1:7890' },
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
  return TTS_PROVIDERS.find((p) => p.id === id);
}
