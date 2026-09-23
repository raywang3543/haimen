import { AGENT_PROVIDERS, type ProviderInfo } from '@/data/agent-providers';
import type { AgentSettings, ApiResponse } from '@/types';
import { apiFetch } from './client';

function ensureData<T>(res: ApiResponse<T>): T {
  if (res.data == null) throw new Error('Empty response');
  return res.data;
}

/** 后端返回的 provider 原始结构（display_name） */
interface RawProvider {
  id: string;
  display_name: string;
}

/**
 * 从注册表拉取 Web 页面展示的 Agent，按 AGENT_PROVIDERS 的顺序排列。
 * 后端仍可注册更多 Agent；页面仅展示静态列表中指定的四个。
 */
export async function getAgentProviders(): Promise<ProviderInfo[]> {
  const res = await apiFetch<ApiResponse<{ providers: RawProvider[] }>>('/api/v1/agent/providers');
  const data = ensureData(res);
  const available = new Map((data.providers ?? []).map((p) => [p.id, p]));
  return AGENT_PROVIDERS.filter((p) => available.has(p.id)).map((p) => ({
    ...p,
    name: available.get(p.id)?.display_name ?? p.name,
  }));
}

export async function getAgentSettings(): Promise<AgentSettings> {
  const res = await apiFetch<ApiResponse<AgentSettings>>('/api/v1/settings/agent');
  return ensureData(res);
}

export async function updateAgentSettings(settings: {
  active_provider?: string;
  providers?: Record<string, Record<string, string>>;
}): Promise<AgentSettings> {
  const res = await apiFetch<ApiResponse<AgentSettings>>('/api/v1/settings/agent', {
    method: 'PUT',
    body: JSON.stringify(settings),
  });
  return ensureData(res);
}

export async function verifyAgentCredentials(
  provider: string,
  fields?: Record<string, string>,
): Promise<{ valid: boolean; message: string }> {
  const res = await apiFetch<ApiResponse<{ valid: boolean; message: string }>>(
    '/api/v1/settings/agent/verify',
    {
      method: 'POST',
      body: JSON.stringify({ provider, fields: fields ?? {} }),
    },
  );
  return ensureData(res);
}
