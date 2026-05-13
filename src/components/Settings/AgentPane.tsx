import { useMemo, useState } from 'react'
import { CheckCircle2, Loader2, XCircle } from 'lucide-react'
import { motion } from 'framer-motion'
import { useTranslation } from 'react-i18next'
import { useAppStore } from '../../stores/appStore'
import { testHermesAgent, testHermesRoute } from '../../lib/tauri'
import { Toggle } from './shared/Toggle'
import { FormField } from './shared/FormField'

type TestState = 'idle' | 'testing' | 'success' | 'error'

// Built-in trigger words supported by the speech path (case-insensitive,
// at the start of the transcript). Hotkey path doesn't need any trigger
// word — recommended for users whose STT mishears English words.
const TRIGGERS = [
  'ask hermes',
  'ask agent',
  'ask claude',
  'ask gemini',
  'hermes',
  'agent',
  'claude',
  'gemini',
]

interface PresetDef {
  value: string
  label: string
  // Default args template shown as placeholder + filled when user picks the preset.
  defaultArgs: string
  // Hint text under the dropdown.
  hint: string
}

const PRESETS: PresetDef[] = [
  {
    value: 'hermes',
    label: 'Hermes',
    defaultArgs: '-z {prompt}',
    hint: 'Auto-discovers `hermes` in $HOME/miniconda3/bin, $HOME/anaconda3/bin, $HOME/.local/bin, $HOME/.cargo/bin. Override below if installed elsewhere.',
  },
  {
    value: 'claude',
    label: 'Claude Code',
    defaultArgs: '--print {prompt}',
    hint: 'Auto-discovers `claude` in $HOME/.npm-global/bin, $HOME/.bun/bin, $HOME/.local/bin. Install via `npm install -g @anthropic-ai/claude-code` or the official installer.',
  },
  {
    value: 'gemini',
    label: 'Gemini CLI',
    defaultArgs: '--prompt {prompt}',
    hint: 'Auto-discovers `gemini` in $HOME/.npm-global/bin, $HOME/.bun/bin, $HOME/.local/bin. Install via `npm install -g @google/gemini-cli`.',
  },
  {
    value: 'custom',
    label: 'Custom',
    defaultArgs: '{prompt}',
    hint: 'Specify your own binary path and args template. Use literal {prompt} where the prompt text should be substituted.',
  },
]

export function AgentPane() {
  const config = useAppStore((s) => s.config)
  const updateConfig = useAppStore((s) => s.updateConfig)
  const { t } = useTranslation()
  const [testState, setTestState] = useState<TestState>('idle')
  const [testMessage, setTestMessage] = useState('')
  const [routeText, setRouteText] = useState('hermes say only agent route test ok')
  const [routeState, setRouteState] = useState<TestState>('idle')
  const [routeMessage, setRouteMessage] = useState('')

  const preset = config.agent_preset || 'hermes'
  const presetDef = PRESETS.find((p) => p.value === preset) ?? PRESETS[0]

  // Show in the runtime preview what'll actually be executed. Empty fields
  // get replaced with the preset's defaults (resolved at backend invocation).
  const runtime = useMemo(() => {
    const cmd = config.agent_command.trim() || `<auto: ${preset}>`
    const args = config.agent_args.trim() || presetDef.defaultArgs
    const cwd = config.agent_cwd.trim() || '(app current directory)'
    return `${cmd} ${args}\n${cwd}`
  }, [config.agent_command, config.agent_args, config.agent_cwd, preset, presetDef])

  const handleTest = async () => {
    setTestState('testing')
    setTestMessage('')
    try {
      const response = await testHermesAgent(config)
      setTestState('success')
      setTestMessage(response)
    } catch (e) {
      setTestState('error')
      setTestMessage(e instanceof Error ? e.message : String(e))
    }
  }

  const handleRouteTest = async () => {
    setRouteState('testing')
    setRouteMessage('')
    try {
      const response = await testHermesRoute(config, routeText)
      setRouteState('success')
      setRouteMessage(response)
    } catch (e) {
      setRouteState('error')
      setRouteMessage(e instanceof Error ? e.message : String(e))
    }
  }

  const handlePresetChange = (newPreset: string) => {
    // When user switches preset, clear `agent_command` and `agent_args` so
    // the backend's auto-discovery + preset default args kick in. Users who
    // had explicit overrides can re-enter them afterwards.
    const newDef = PRESETS.find((p) => p.value === newPreset)
    updateConfig({
      agent_preset: newPreset,
      agent_command: '',
      agent_args: newPreset === 'custom' ? (newDef?.defaultArgs ?? '{prompt}') : '',
    })
  }

  return (
    <div className="space-y-6">
      <Section title={t('settings.agentRuntime')}>
        <div className="space-y-4">
          <Toggle
            checked={config.agent_enabled}
            onChange={(checked) => updateConfig({ agent_enabled: checked })}
            label={t('settings.enableAgent')}
          />

          <FormField label={t('settings.agentPreset', { defaultValue: 'Agent' })}>
            <select
              value={preset}
              onChange={(e) => handlePresetChange(e.target.value)}
              className="w-full px-3 py-2.5 bg-bg-secondary border border-border rounded-[10px] text-[13px] text-text-primary outline-none focus:border-border-focus transition-colors"
            >
              {PRESETS.map((p) => (
                <option key={p.value} value={p.value}>
                  {p.label}
                </option>
              ))}
            </select>
            <p className="text-[11px] text-text-tertiary mt-1.5">{presetDef.hint}</p>
          </FormField>

          <FormField
            label={t('settings.agentCommand', { defaultValue: 'Command (override)' })}
          >
            <input
              value={config.agent_command}
              onChange={(e) => updateConfig({ agent_command: e.target.value })}
              placeholder={
                preset === 'custom'
                  ? '/absolute/path/to/your-agent'
                  : `auto-resolved: ${presetDef.value}`
              }
              className="w-full px-3 py-2.5 rounded-[10px] bg-bg-secondary text-text-primary border border-transparent focus:border-accent outline-none text-[13px] font-mono"
            />
            <p className="text-[11px] text-text-tertiary mt-1.5">
              {t('settings.agentCommandHint', {
                defaultValue:
                  'Leave empty to auto-discover. Set an absolute path if the binary is not in a standard location.',
              })}
            </p>
          </FormField>

          <FormField
            label={t('settings.agentArgs', { defaultValue: 'Args template' })}
          >
            <input
              value={config.agent_args}
              onChange={(e) => updateConfig({ agent_args: e.target.value })}
              placeholder={presetDef.defaultArgs}
              className="w-full px-3 py-2.5 rounded-[10px] bg-bg-secondary text-text-primary border border-transparent focus:border-accent outline-none text-[13px] font-mono"
            />
            <p className="text-[11px] text-text-tertiary mt-1.5">
              {t('settings.agentArgsHint', {
                defaultValue:
                  'Use literal {prompt} as placeholder for the prompt text. Empty = use preset default.',
              })}
            </p>
          </FormField>

          <FormField label={t('settings.hermesWorkingDirectory')}>
            <input
              value={config.agent_cwd}
              onChange={(e) => updateConfig({ agent_cwd: e.target.value })}
              placeholder={t('settings.hermesCwdPlaceholder')}
              className="w-full px-3 py-2.5 rounded-[10px] bg-bg-secondary text-text-primary border border-transparent focus:border-accent outline-none text-[13px] font-mono"
            />
          </FormField>
        </div>
      </Section>

      <Section title={t('settings.agentRouting')}>
        <div className="space-y-3">
          <div className="flex flex-wrap gap-2">
            {TRIGGERS.map((trigger) => (
              <span
                key={trigger}
                className="px-2.5 py-1 rounded-[8px] bg-bg-secondary text-[12px] text-text-secondary font-mono"
              >
                {trigger}
              </span>
            ))}
          </div>
          <p className="text-[12px] text-text-tertiary leading-relaxed">
            {t('settings.agentRoutingDesc')}
          </p>
        </div>
      </Section>

      <Section title={t('settings.currentHermesRequest', { defaultValue: 'Resolved command' })}>
        <pre className="whitespace-pre-wrap break-words rounded-[10px] bg-bg-secondary px-3 py-2.5 text-[12px] text-text-secondary font-mono">
          {runtime}
        </pre>
      </Section>

      <Section title={t('settings.testHermes', { defaultValue: 'Test agent' })}>
        <div className="space-y-3">
          <button
            onClick={handleTest}
            disabled={testState === 'testing'}
            className="inline-flex items-center gap-2 px-3 py-2 rounded-[10px] bg-accent text-white text-[13px] font-medium border-none cursor-pointer hover:opacity-90 disabled:opacity-70"
          >
            {testState === 'testing' && (
              <motion.span
                animate={{ rotate: 360 }}
                transition={{ repeat: Infinity, duration: 0.8, ease: 'linear' }}
              >
                <Loader2 size={14} />
              </motion.span>
            )}
            {t('settings.runHermesTest', { defaultValue: 'Run test' })}
          </button>

          {testState === 'success' && (
            <Status tone="success" text={testMessage || t('settings.connectionSuccess')} />
          )}
          {testState === 'error' && (
            <Status tone="error" text={testMessage || t('settings.connectionFailed')} />
          )}
        </div>
      </Section>

      <Section title={t('settings.testHermesRoute', { defaultValue: 'Test routing' })}>
        <div className="space-y-3">
          <textarea
            value={routeText}
            onChange={(e) => setRouteText(e.target.value)}
            className="w-full min-h-[72px] resize-none px-3 py-2.5 rounded-[10px] bg-bg-secondary text-text-primary border border-transparent focus:border-accent outline-none text-[13px]"
          />
          <button
            onClick={handleRouteTest}
            disabled={routeState === 'testing'}
            className="inline-flex items-center gap-2 px-3 py-2 rounded-[10px] bg-accent text-white text-[13px] font-medium border-none cursor-pointer hover:opacity-90 disabled:opacity-70"
          >
            {routeState === 'testing' && (
              <motion.span
                animate={{ rotate: 360 }}
                transition={{ repeat: Infinity, duration: 0.8, ease: 'linear' }}
              >
                <Loader2 size={14} />
              </motion.span>
            )}
            {t('settings.runHermesRouteTest', { defaultValue: 'Run route test' })}
          </button>

          {routeState === 'success' && (
            <Status tone="success" text={routeMessage || t('settings.connectionSuccess')} />
          )}
          {routeState === 'error' && (
            <Status tone="error" text={routeMessage || t('settings.connectionFailed')} />
          )}
        </div>
      </Section>
    </div>
  )
}

function Status({ tone, text }: { tone: 'success' | 'error'; text: string }) {
  const Icon = tone === 'success' ? CheckCircle2 : XCircle
  const color = tone === 'success' ? 'text-success' : 'text-error'
  return (
    <div className={`flex items-start gap-2 text-[12px] ${color}`}>
      <Icon size={14} className="mt-0.5 flex-shrink-0" />
      <span className="break-words">{text}</span>
    </div>
  )
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div>
      <h3 className="text-[11px] font-medium text-text-tertiary uppercase tracking-wider mb-2.5">
        {title}
      </h3>
      {children}
    </div>
  )
}
