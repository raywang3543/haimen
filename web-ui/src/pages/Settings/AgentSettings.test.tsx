import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { expect, it, vi } from 'vitest';
import { getAgentProviders, getAgentSettings, updateAgentSettings } from '@/api/agent';
import { AGENT_PROVIDERS } from '@/data/agent-providers';
import AgentSettings from './AgentSettings';

vi.mock('@/api/agent', () => ({
  getAgentProviders: vi.fn(),
  getAgentSettings: vi.fn(),
  updateAgentSettings: vi.fn(),
  verifyAgentCredentials: vi.fn(),
}));

it('保存 Codex 模型与强度，保留其他配置，并允许恢复默认', async () => {
  const settings = {
    active_provider: 'codex',
    providers: {
      codex: { cli_path: '/opt/codex', sandbox: 'workspace-write', model: 'old-model' },
      ollama: { model_id: 'other-model' },
    },
  };
  vi.mocked(getAgentProviders).mockResolvedValue(AGENT_PROVIDERS);
  vi.mocked(getAgentSettings).mockResolvedValue(settings);
  vi.mocked(updateAgentSettings).mockImplementation(async (next) => ({
    ...settings,
    ...next,
  }));
  render(<AgentSettings />);
  const model = await screen.findByLabelText('模型 ID');
  fireEvent.change(screen.getByLabelText('工作空间目录'), {
    target: { value: '~/projects/plain-folder' },
  });
  fireEvent.change(model, { target: { value: 'custom-model' } });
  fireEvent.change(screen.getByLabelText('思考强度'), { target: { value: 'high' } });
  fireEvent.click(screen.getByRole('button', { name: '保存配置' }));
  await waitFor(() =>
    expect(updateAgentSettings).toHaveBeenCalledWith({
      active_provider: 'codex',
      providers: {
        ...settings.providers,
        codex: {
          ...settings.providers.codex,
          model: 'custom-model',
          work_dir: '~/projects/plain-folder',
          model_reasoning_effort: 'high',
        },
      },
    }),
  );
  await waitFor(() =>
    expect(screen.getByRole('button', { name: '保存配置' }).hasAttribute('disabled')).toBe(false),
  );
  fireEvent.change(screen.getByLabelText('模型 ID'), { target: { value: '' } });
  fireEvent.change(screen.getByLabelText('思考强度'), { target: { value: '' } });
  fireEvent.click(screen.getByRole('button', { name: '保存配置' }));
  await waitFor(() =>
    expect(updateAgentSettings).toHaveBeenLastCalledWith({
      active_provider: 'codex',
      providers: {
        ...settings.providers,
        codex: {
          ...settings.providers.codex,
          model: '',
          model_reasoning_effort: '',
        },
      },
    }),
  );
});
