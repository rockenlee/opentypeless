import { create } from 'zustand'

export type PipelineState = 'idle' | 'recording' | 'transcribing' | 'polishing' | 'outputting'

export type SttProvider =
  | 'deepgram'
  | 'assemblyai'
  | 'glm-asr'
  | 'openai-whisper'
  | 'groq-whisper'
  | 'siliconflow'
  | 'cloud'
export type LlmProvider =
  | 'zhipu'
  | 'deepseek'
  | 'siliconflow'
  | 'openai'
  | 'gemini'
  | 'moonshot'
  | 'qwen'
  | 'groq'
  | 'claude'
  | 'ollama'
  | 'openrouter'
  | 'cloud'
export type OutputMode = 'keyboard' | 'clipboard'
export type HotkeyMode = 'hold' | 'toggle'
export type Theme = 'light' | 'dark' | 'system'

export interface HistoryEntry {
  id: number
  created_at: string
  app_name: string
  app_type: string
  raw_text: string
  polished_text: string
  language: string | null
  duration_ms: number | null
  agent_response: string | null
}

export interface DictionaryEntry {
  id: number
  word: string
  pronunciation: string | null
}

export interface AppConfig {
  stt_provider: SttProvider
  stt_api_key: string
  stt_language: string
  llm_provider: LlmProvider
  llm_api_key: string
  llm_model: string
  llm_base_url: string
  polish_enabled: boolean
  translate_enabled: boolean
  target_lang: string
  hotkey: string
  hotkey_mode: HotkeyMode
  output_mode: OutputMode
  selected_text_enabled: boolean
  theme: Theme
  auto_start: boolean
  close_to_tray: boolean
  start_minimized: boolean
  max_recording_seconds: number
  ui_language: string
  capsule_auto_hide: boolean
  // Agent integration (generic local-CLI). User picks a preset from
  // {hermes, claude, gemini, custom}; presets supply default command + args.
  // Any field can be overridden manually.
  agent_enabled: boolean
  agent_preset: string
  agent_command: string
  /** CLI args template; literal {prompt} is replaced with the prompt text. */
  agent_args: string
  agent_cwd: string
  /** Hotkey that toggles `translate_enabled`. Empty string = unbound. */
  translate_hotkey: string
  /** Hotkey that starts/stops a forced Hermes-agent recording (no trigger
   *  word required, whole transcript becomes the prompt). Empty = unbound. */
  agent_hotkey: string
  /** Pause locally-playing music / podcasts / video on record start. */
  auto_pause_media: boolean
  /** Show a native macOS notification when an agent run completes. */
  agent_notification: boolean
}

export type TestStatus = 'idle' | 'testing' | 'success' | 'error'

interface AppState {
  // Pipeline
  pipelineState: PipelineState
  setPipelineState: (state: PipelineState) => void

  // Recording
  audioVolume: number
  setAudioVolume: (v: number) => void
  partialTranscript: string
  setPartialTranscript: (t: string) => void
  finalTranscript: string
  setFinalTranscript: (t: string) => void
  polishedText: string
  setPolishedText: (t: string) => void
  appendPolishedChunk: (chunk: string) => void
  recordingDuration: number
  setRecordingDuration: (d: number) => void
  targetApp: string
  setTargetApp: (app: string) => void

  // Config
  config: AppConfig
  setConfig: (config: AppConfig) => void
  updateConfig: (partial: Partial<AppConfig>) => void

  // History
  history: HistoryEntry[]
  setHistory: (h: HistoryEntry[]) => void

  // Dictionary
  dictionary: DictionaryEntry[]
  setDictionary: (d: DictionaryEntry[]) => void

  // Onboarding
  onboardingCompleted: boolean
  setOnboardingCompleted: (done: boolean) => void
  onboardingStep: number
  setOnboardingStep: (step: number) => void
  onboardingMode: 'cloud' | 'byok' | null
  setOnboardingMode: (mode: 'cloud' | 'byok' | null) => void

  // Capsule
  capsuleExpanded: boolean
  setCapsuleExpanded: (expanded: boolean) => void

  // Connection test status
  sttTestStatus: TestStatus
  setSttTestStatus: (s: TestStatus) => void
  llmTestStatus: TestStatus
  setLlmTestStatus: (s: TestStatus) => void

  // Latency benchmark results (ms), null = not yet measured
  sttLatencyMs: number | null
  setSttLatencyMs: (ms: number | null) => void
  llmLatencyMs: number | null
  setLlmLatencyMs: (ms: number | null) => void

  // LLM model list cache (persists across tab switches)
  llmModels: string[]
  setLlmModels: (models: string[]) => void

  // Agent runtime status
  agentStatus: string
  setAgentStatus: (status: string) => void

  // Agent result panel
  agentResult: string | null
  setAgentResult: (result: string | null) => void

  // Pipeline error
  pipelineError: string | null
  setPipelineError: (error: string | null) => void

  // macOS Accessibility permission
  accessibilityTrusted: boolean
  setAccessibilityTrusted: (trusted: boolean) => void

  // Context menu
  contextMenuOpen: boolean
  setContextMenuOpen: (open: boolean) => void
  contextMenuReady: boolean
  setContextMenuReady: (ready: boolean) => void

  // Reset recording state
  resetRecording: () => void

  // Config snapshot for dirty detection
  savedConfig: AppConfig | null
  setSavedConfig: (config: AppConfig) => void
  resetConfig: () => void
}

const isMac =
  typeof navigator !== 'undefined' && navigator.platform.toUpperCase().indexOf('MAC') >= 0

const defaultConfig: AppConfig = {
  stt_provider: 'glm-asr',
  stt_api_key: '',
  stt_language: 'multi',
  llm_provider: 'openrouter',
  llm_api_key: '',
  llm_model: 'google/gemini-2.5-flash',
  llm_base_url: 'https://openrouter.ai/api/v1',
  polish_enabled: true,
  translate_enabled: false,
  target_lang: 'en',
  hotkey: isMac ? 'Alt+/' : 'Ctrl+/',
  hotkey_mode: 'hold',
  output_mode: 'keyboard',
  selected_text_enabled: false,
  theme: 'system',
  auto_start: false,
  close_to_tray: true,
  start_minimized: false,
  max_recording_seconds: 30,
  ui_language: 'en',
  capsule_auto_hide: false,
  // Agent integration. Frontend defaults are minimal — the Rust backend
  // resolves the real per-preset binary path at runtime via $HOME-based
  // candidate paths (no hard-coded usernames), so the JS bundle is shareable.
  agent_enabled: true,
  agent_preset: 'hermes',
  agent_command: '',
  agent_args: '',
  agent_cwd: '',
  translate_hotkey: isMac ? 'Alt+Shift+.' : 'Ctrl+Shift+.',
  agent_hotkey: isMac ? 'Alt+Shift+/' : 'Ctrl+Shift+/',
  auto_pause_media: true,
  agent_notification: true,
}

export const useAppStore = create<AppState>((set) => ({
  pipelineState: 'idle',
  setPipelineState: (pipelineState) => set({ pipelineState }),

  audioVolume: 0,
  setAudioVolume: (audioVolume) => set({ audioVolume }),
  partialTranscript: '',
  setPartialTranscript: (partialTranscript) => set({ partialTranscript }),
  finalTranscript: '',
  setFinalTranscript: (finalTranscript) => set({ finalTranscript }),
  polishedText: '',
  setPolishedText: (polishedText) => set({ polishedText }),
  appendPolishedChunk: (chunk) => set((s) => ({ polishedText: s.polishedText + chunk })),
  recordingDuration: 0,
  setRecordingDuration: (recordingDuration) => set({ recordingDuration }),
  targetApp: '',
  setTargetApp: (targetApp) => set({ targetApp }),

  config: defaultConfig,
  setConfig: (config) => set({ config }),
  updateConfig: (partial) => set((s) => ({ config: { ...s.config, ...partial } })),

  history: [],
  setHistory: (history) => set({ history }),

  dictionary: [],
  setDictionary: (dictionary) => set({ dictionary }),

  onboardingCompleted: false,
  setOnboardingCompleted: (onboardingCompleted) => set({ onboardingCompleted }),
  onboardingStep: 0,
  setOnboardingStep: (onboardingStep) => set({ onboardingStep }),
  onboardingMode: null,
  setOnboardingMode: (onboardingMode) => set({ onboardingMode }),

  capsuleExpanded: false,
  setCapsuleExpanded: (capsuleExpanded) => set({ capsuleExpanded }),

  sttTestStatus: 'idle',
  setSttTestStatus: (sttTestStatus) => set({ sttTestStatus }),
  llmTestStatus: 'idle',
  setLlmTestStatus: (llmTestStatus) => set({ llmTestStatus }),

  sttLatencyMs: null,
  setSttLatencyMs: (sttLatencyMs) => set({ sttLatencyMs }),
  llmLatencyMs: null,
  setLlmLatencyMs: (llmLatencyMs) => set({ llmLatencyMs }),

  llmModels: [],
  setLlmModels: (llmModels) => set({ llmModels }),

  agentStatus: '',
  setAgentStatus: (agentStatus) => set({ agentStatus }),

  agentResult: null,
  setAgentResult: (agentResult) => set({ agentResult }),

  pipelineError: null,
  setPipelineError: (pipelineError) => set({ pipelineError }),

  accessibilityTrusted: true,
  setAccessibilityTrusted: (accessibilityTrusted) => set({ accessibilityTrusted }),

  contextMenuOpen: false,
  setContextMenuOpen: (contextMenuOpen) => set({ contextMenuOpen }),
  contextMenuReady: false,
  setContextMenuReady: (contextMenuReady) => set({ contextMenuReady }),

  resetRecording: () =>
    set({
      audioVolume: 0,
      partialTranscript: '',
      finalTranscript: '',
      polishedText: '',
      recordingDuration: 0,
    }),

  savedConfig: null,
  setSavedConfig: (savedConfig) => set({ savedConfig }),
  resetConfig: () => set((s) => (s.savedConfig ? { config: { ...s.savedConfig } } : {})),
}))
