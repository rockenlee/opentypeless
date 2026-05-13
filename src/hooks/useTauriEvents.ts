import { useEffect } from 'react'
import { listen } from '@tauri-apps/api/event'
import { useAppStore } from '../stores/appStore'
import type { PipelineState } from '../stores/appStore'
import { getHistory } from '../lib/tauri'
import { toast } from '../components/Toast'

const LANG_LABEL: Record<string, string> = {
  en: 'English',
  zh: '中文',
  ja: '日本語',
  ko: '한국어',
  fr: 'Français',
  de: 'Deutsch',
  es: 'Español',
  pt: 'Português',
  ru: 'Русский',
  ar: 'العربية',
  hi: 'हिन्दी',
  th: 'ไทย',
  vi: 'Tiếng Việt',
  it: 'Italiano',
  nl: 'Nederlands',
  tr: 'Türkçe',
  pl: 'Polski',
  uk: 'Українська',
  id: 'Bahasa Indonesia',
  ms: 'Bahasa Melayu',
}

export function useTauriEvents() {
  const {
    setAudioVolume,
    setPartialTranscript,
    setFinalTranscript,
    appendPolishedChunk,
    setPipelineState,
    setTargetApp,
    setPipelineError,
    setAccessibilityTrusted,
    setHistory,
    setAgentStatus,
    setAgentResult,
    resetRecording,
    updateConfig,
  } = useAppStore()

  useEffect(() => {
    let cancelled = false
    const unlisteners: Array<() => void> = []

    function addListener<T>(event: string, handler: (payload: T) => void) {
      listen<T>(event, (e) => handler(e.payload))
        .then((unlisten) => {
          if (cancelled) {
            unlisten()
          } else {
            unlisteners.push(unlisten)
          }
        })
        .catch((err) => {
          console.error(`Failed to register listener for "${event}":`, err)
        })
    }

    addListener<number>('audio:volume', setAudioVolume)
    addListener<string>('stt:partial', setPartialTranscript)
    addListener<string>('stt:final', setFinalTranscript)
    addListener<string>('llm:chunk', appendPolishedChunk)
    addListener<string>('agent:status', setAgentStatus)
    addListener<PipelineState>('pipeline:state', (state) => {
      setPipelineState(state)
      if (state === 'recording') {
        // Clear any previous error and agent result when starting a new pipeline run
        resetRecording()
        setPipelineError(null)
        setAgentStatus('')
        setAgentResult(null)
      }
      if (state === 'idle') {
        // Don't clear pipelineError here — CapsuleError auto-resets after 2.5s.
        // Clearing here would swallow errors from failed start() calls that
        // transition Recording → Idle in rapid succession.
        getHistory(200, 0)
          .then(setHistory)
          .catch((err) => {
            console.error('Failed to refresh history:', err)
          })
      }
    })
    addListener<string>('pipeline:target_app', setTargetApp)
    addListener<string>('pipeline:error', (error) => {
      console.error('[pipeline:error]', error)
      setPipelineError(error)
      if (error === 'ACCESSIBILITY_REQUIRED') {
        setAccessibilityTrusted(false)
      }
    })

    // Translate hotkey was pressed (toggle). Backend has already flipped
    // translate_enabled and persisted it — we just refresh our local mirror
    // of config and surface a toast so the user knows their press registered.
    addListener<{ enabled: boolean; target_lang: string }>(
      'translate:toggled',
      ({ enabled, target_lang }) => {
        updateConfig({ translate_enabled: enabled, target_lang })
        const langLabel = LANG_LABEL[target_lang] ?? target_lang
        toast(enabled ? `Translate ON → ${langLabel}` : 'Translate OFF', 'info')
      },
    )

    addListener<void>('tray:settings', () => {
      window.location.hash = '#/settings'
    })
    addListener<void>('tray:history', () => {
      window.location.hash = '#/history'
    })
    addListener<string>('navigate', (hash) => {
      window.location.hash = hash
    })
    addListener<void>('tray:about', () => {
      window.location.hash = '#/settings'
    })

    return () => {
      cancelled = true
      unlisteners.forEach((unlisten) => unlisten())
    }
  }, [
    setAudioVolume,
    setPartialTranscript,
    setFinalTranscript,
    appendPolishedChunk,
    setPipelineState,
    setTargetApp,
    setPipelineError,
    setAccessibilityTrusted,
    setHistory,
    setAgentStatus,
    setAgentResult,
    resetRecording,
    updateConfig,
  ])
}
