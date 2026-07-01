import { useEffect } from 'react'
import { listen } from '@tauri-apps/api/event'
import { useAppStore } from '../stores/appStore'
import type { PipelineState } from '../stores/appStore'
import { getHistory, notifyEditSelection } from '../lib/tauri'
import { toast } from '../components/Toast'
import i18n from '../i18n'

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
    setEditSelection,
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
        // Clear any previous error and agent result when starting a new pipeline run.
        // Also clear stale Edit Selection mode; a real Edit Selection run re-arms it
        // via the edit_selection:active event that follows this one.
        resetRecording()
        setPipelineError(null)
        setAgentStatus('')
        setAgentResult(null)
        setEditSelection({ active: false, chars: 0, words: 0 })
      }
      if (state === 'idle') {
        setEditSelection({ active: false, chars: 0, words: 0 })
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

    // ── Edit Selection mode visibility + localized outcomes ──
    // The backend never sends user-facing text for Edit Selection — it sends a
    // mode flag, a selection size, and a coded result. We localize here so the
    // messages honour the user's UI language.
    addListener<{ active: boolean }>('edit_selection:active', ({ active }) => {
      setEditSelection({ active, chars: 0, words: 0 })
    })
    addListener<{ chars: number; words: number }>('edit_selection:size', ({ chars, words }) => {
      const cur = useAppStore.getState().editSelection
      setEditSelection({ active: cur.active, chars, words })
    })
    addListener<{ status: string; code: string; app: string }>(
      'edit_selection:result',
      ({ status, code, app }) => {
        // Leaving Edit Selection mode regardless of outcome.
        setEditSelection({ active: false, chars: 0, words: 0 })
        if (status === 'success') {
          const successMsg = i18n.t('editSelection.result.success')
          toast(successMsg, 'success')
          // The user is focused on their target app, so the toast is on the
          // hidden main window — also fire a native notification (undo hint).
          void notifyEditSelection(successMsg)
          return
        }
        // fail / fallback → build a localized, app-interpolated message and show
        // it as a toast (main window), the capsule error surface, AND a native
        // notification — the clipboard-fallback guidance is otherwise invisible
        // while the user is focused on their target app.
        const key = `editSelection.error.${code}`
        let msg = i18n.t(key, { app })
        if (msg === key) msg = i18n.t('editSelection.error.generic')
        toast(msg, 'error')
        setPipelineError(msg)
        void notifyEditSelection(msg)
      },
    )

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

    addListener<boolean>('mouse:tap_active', (active) => {
      if (!active) {
        toast(
          '鼠标触发不可用：请在系统设置 → 隐私与安全性 → 辅助功能中删除 OpenTypeless 再重新添加',
          'error',
        )
      }
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
    setEditSelection,
    resetRecording,
    updateConfig,
  ])
}
