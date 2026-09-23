import { describe, expect, it, vi } from 'vitest';
import { getAgentProviders } from './agent';
import { apiFetch } from './client';

vi.mock('./client', () => ({ apiFetch: vi.fn() }));

describe('getAgentProviders', () => {
  it('仅展示四个 Agent，并按页面指定顺序排列', async () => {
    vi.mocked(apiFetch).mockResolvedValueOnce({
      success: true,
      data: {
        providers: [
          { id: 'hermes', display_name: 'Hermes' },
          { id: 'custom', display_name: '自定义 Agent' },
          { id: 'ollama', display_name: 'Ollama' },
          { id: 'claude-code', display_name: 'Claude Code' },
          { id: 'codex', display_name: 'Codex CLI' },
          { id: 'openclaw', display_name: 'OpenClaw' },
        ],
      },
    });

    const providers = await getAgentProviders();
    expect(providers.map((provider) => provider.id)).toEqual([
      'openclaw',
      'codex',
      'ollama',
      'custom',
    ]);
    expect(
      providers.find((provider) => provider.id === 'custom')?.fields.map((f) => f.key),
    ).toEqual(['base_url', 'model_id', 'api_key']);
  });
});
