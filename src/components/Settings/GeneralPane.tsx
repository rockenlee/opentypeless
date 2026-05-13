import { useState, useCallback, useEffect, useRef } from 'react'
import { useTranslation } from 'react-i18next'
import { useAppStore } from '../../stores/appStore'
import type { HotkeyMode, OutputMode } from '../../stores/appStore'
import { updateHotkey, updateTranslateHotkey, updateAgentHotkey, pauseHotkey, resumeHotkey, checkAccessibilityPermission, requestAccessibilityPermission } from '../../lib/tauri'
import { TARGET_LANGUAGES } from '../../lib/constants'
import { toast } from '../Toast'
import { SegmentedControl } from './shared/SegmentedControl'
import { Toggle } from './shared/Toggle'
import { FormField } from './shared/FormField'

// Map W3C KeyboardEvent.code (physical key, unaffected by Shift / Option /
// language layout / IME) to the canonical key name that the Rust `parse_hotkey`
// understands. Using `e.code` is the only way to reliably identify the key:
// `e.key` returns the typed character, which on macOS gets mangled by Option
// (e.g. Option+/ → "÷", Option+. → "≥") and by Shift (Shift+. → ">").
const CODE_TO_NAME: Record<string, string> = {
  // Letters
  KeyA: 'A', KeyB: 'B', KeyC: 'C', KeyD: 'D', KeyE: 'E', KeyF: 'F', KeyG: 'G',
  KeyH: 'H', KeyI: 'I', KeyJ: 'J', KeyK: 'K', KeyL: 'L', KeyM: 'M', KeyN: 'N',
  KeyO: 'O', KeyP: 'P', KeyQ: 'Q', KeyR: 'R', KeyS: 'S', KeyT: 'T', KeyU: 'U',
  KeyV: 'V', KeyW: 'W', KeyX: 'X', KeyY: 'Y', KeyZ: 'Z',
  // Digits (top row)
  Digit0: '0', Digit1: '1', Digit2: '2', Digit3: '3', Digit4: '4',
  Digit5: '5', Digit6: '6', Digit7: '7', Digit8: '8', Digit9: '9',
  // Symbols
  Period: '.', Comma: ',', Slash: '/', Backslash: '\\',
  Semicolon: ';', Quote: "'", Backquote: '`',
  Minus: '-', Equal: '=',
  BracketLeft: '[', BracketRight: ']',
  // Navigation / function
  Space: 'Space', Tab: 'Tab', Enter: 'Enter', Backspace: 'Backspace',
  Escape: 'Escape', Delete: 'Delete', Insert: 'Insert',
  Home: 'Home', End: 'End', PageUp: 'PageUp', PageDown: 'PageDown',
  ArrowUp: 'Up', ArrowDown: 'Down', ArrowLeft: 'Left', ArrowRight: 'Right',
  F1: 'F1', F2: 'F2', F3: 'F3', F4: 'F4', F5: 'F5', F6: 'F6',
  F7: 'F7', F8: 'F8', F9: 'F9', F10: 'F10', F11: 'F11', F12: 'F12',
  // macOS Return key sometimes reports as NumpadEnter
  NumpadEnter: 'Enter',
}

// Keys that can be used as hotkeys without a modifier
const STANDALONE_KEYS = new Set([
  'Space',
  'Tab',
  'Enter',
  'Backspace',
  'Escape',
  'Delete',
  'Insert',
  'Home',
  'End',
  'PageUp',
  'PageDown',
  'Up',
  'Down',
  'Left',
  'Right',
  'F1',
  'F2',
  'F3',
  'F4',
  'F5',
  'F6',
  'F7',
  'F8',
  'F9',
  'F10',
  'F11',
  'F12',
])

type HotkeyKind = 'recording' | 'translate' | 'agent'

function HotkeyRecorder({ kind = 'recording' }: { kind?: HotkeyKind }) {
  const config = useAppStore((s) => s.config)
  const updateConfig = useAppStore((s) => s.updateConfig)
  const { t } = useTranslation()
  const [recording, setRecording] = useState(false)
  const [pending, setPending] = useState<string | null>(null)
  const [modifierHint, setModifierHint] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const autoConfirmTimer = useRef<ReturnType<typeof setTimeout> | null>(null)

  const currentValue =
    kind === 'translate'
      ? config.translate_hotkey
      : kind === 'agent'
        ? config.agent_hotkey
        : config.hotkey
  const placeholder = currentValue || t('settings.notSet')

  const confirmHotkey = useCallback(
    (hotkey: string) => {
      setRecording(false)
      setError(null)
      setModifierHint(null)
      const persist =
        kind === 'translate'
          ? updateTranslateHotkey
          : kind === 'agent'
            ? updateAgentHotkey
            : updateHotkey
      persist(hotkey)
        .then(() => {
          updateConfig(
            kind === 'translate'
              ? { translate_hotkey: hotkey }
              : kind === 'agent'
                ? { agent_hotkey: hotkey }
                : { hotkey },
          )
          setPending(null)
          toast(`Hotkey saved: ${hotkey}`, 'success')
        })
        .catch((e) => {
          // Surface the actual backend error (e.g. "Invalid hotkey: Alt+Shift+>")
          // both inline AND as a toast — previously errors were only set on local
          // state, so the user only saw a tiny red line and easily missed it.
          const msg = String(e)
          setError(msg)
          setPending(null)
          toast(msg, 'error')
          resumeHotkey().catch(() => {})
        })
    },
    [updateConfig, kind],
  )

  const handleKeyDown = useCallback(
    (e: KeyboardEvent) => {
      e.preventDefault()
      e.stopPropagation()

      // Build modifier prefix
      const parts: string[] = []
      if (e.ctrlKey) parts.push('Ctrl')
      if (e.altKey) parts.push('Alt')
      if (e.shiftKey) parts.push('Shift')
      if (e.metaKey) parts.push('Meta')

      // If only modifier keys are pressed, show hint like "Alt+..."
      // Modifier-key codes are stable across layouts and modifier state.
      const MODIFIER_CODES = new Set([
        'ControlLeft', 'ControlRight',
        'ShiftLeft', 'ShiftRight',
        'AltLeft', 'AltRight',
        'MetaLeft', 'MetaRight',
      ])
      if (MODIFIER_CODES.has(e.code)) {
        setModifierHint(parts.length > 0 ? parts.join('+') + '+...' : null)
        return
      }

      setModifierHint(null)

      // Use `e.code` (physical key position), NOT `e.key` (typed character).
      // On macOS, Option+/ produces "÷" in `e.key` and Shift+. produces ">".
      // The Rust parser keys off the unshifted symbol, so we'd need to undo
      // every modifier's text mangling. Physical code is unambiguous.
      const keyName = CODE_TO_NAME[e.code]
      if (!keyName) {
        // Unknown physical key (e.g. media keys, IntlBackslash). Don't accept.
        return
      }

      // Letters and digits require at least one modifier to avoid interfering with typing
      if (parts.length === 0 && !STANDALONE_KEYS.has(keyName)) return

      parts.push(keyName)
      const combo = parts.join('+')
      setPending(combo)

      // Auto-confirm after 1.5 seconds
      if (autoConfirmTimer.current) clearTimeout(autoConfirmTimer.current)
      autoConfirmTimer.current = setTimeout(() => {
        confirmHotkey(combo)
      }, 1500)
    },
    [confirmHotkey],
  )

  const handleKeyUp = useCallback(() => {
    setModifierHint(null)
  }, [])

  useEffect(() => {
    if (!recording) return
    window.addEventListener('keydown', handleKeyDown, true)
    window.addEventListener('keyup', handleKeyUp, true)
    return () => {
      window.removeEventListener('keydown', handleKeyDown, true)
      window.removeEventListener('keyup', handleKeyUp, true)
      if (autoConfirmTimer.current) clearTimeout(autoConfirmTimer.current)
    }
  }, [recording, handleKeyDown, handleKeyUp])

  const handleClick = () => {
    if (recording && pending) {
      // Confirm immediately on click
      if (autoConfirmTimer.current) clearTimeout(autoConfirmTimer.current)
      confirmHotkey(pending)
    } else if (recording) {
      // Cancel recording — re-register the old hotkey
      setRecording(false)
      setPending(null)
      setModifierHint(null)
      if (autoConfirmTimer.current) clearTimeout(autoConfirmTimer.current)
      resumeHotkey().catch(() => {})
    } else {
      // Start recording — unregister global shortcut so webview can capture keys
      pauseHotkey().catch(() => {})
      setRecording(true)
      setPending(null)
      setError(null)
    }
  }

  return (
    <div>
      <button
        onClick={handleClick}
        className={`w-full px-3 py-2.5 rounded-[10px] text-[13px] font-mono text-left border transition-colors cursor-pointer ${
          recording
            ? 'bg-bg-tertiary border-text-secondary text-text-primary ring-2 ring-text-secondary/20'
            : 'bg-bg-secondary border-transparent text-text-primary hover:border-border'
        }`}
      >
        {recording ? pending || modifierHint || t('settings.pressKeyCombination') : placeholder}
      </button>
      {recording && pending && (
        <p className="text-[11px] text-text-tertiary mt-1.5">{t('settings.clickToConfirm')}</p>
      )}
      {error && <p className="text-[11px] text-error mt-1.5">{error}</p>}
    </div>
  )
}

export function GeneralPane() {
  const config = useAppStore((s) => s.config)
  const updateConfig = useAppStore((s) => s.updateConfig)
  const { t } = useTranslation()
  const isMac = typeof navigator !== 'undefined' && navigator.platform.toUpperCase().indexOf('MAC') >= 0
  const [a11yTrusted, setA11yTrusted] = useState<boolean | null>(null)

  useEffect(() => {
    if (isMac && config.output_mode === 'keyboard') {
      checkAccessibilityPermission().then(setA11yTrusted)
      const onFocus = () => checkAccessibilityPermission().then(setA11yTrusted)
      window.addEventListener('focus', onFocus)
      return () => window.removeEventListener('focus', onFocus)
    }
  }, [isMac, config.output_mode])

  const handleGrantPermission = useCallback(async () => {
    await requestAccessibilityPermission()
    const trusted = await checkAccessibilityPermission()
    setA11yTrusted(trusted)
  }, [])

  return (
    <div className="space-y-6">
      <Section title={t('settings.hotkey')}>
        <HotkeyRecorder />
        <div className="mt-3">
          <SegmentedControl
            options={[
              { value: 'hold', label: t('settings.holdToTalk') },
              { value: 'toggle', label: t('settings.toggleOnOff') },
            ]}
            value={config.hotkey_mode}
            onChange={(v) => updateConfig({ hotkey_mode: v as HotkeyMode })}
          />
        </div>
      </Section>

      <Section title={t('settings.translateHotkey')}>
        <p className="text-[11px] text-text-tertiary mb-2">
          {t('settings.translateHotkeyDesc')}
        </p>
        <HotkeyRecorder kind="translate" />
        <div className="mt-3">
          <FormField label={t('settings.targetLanguage')}>
            <select
              value={config.target_lang}
              onChange={(e) => updateConfig({ target_lang: e.target.value })}
              className="w-full px-3 py-2.5 bg-bg-secondary border border-border rounded-[10px] text-[13px] text-text-primary outline-none focus:border-border-focus transition-colors"
            >
              {TARGET_LANGUAGES.map((l) => (
                <option key={l.value} value={l.value}>
                  {l.label}
                </option>
              ))}
            </select>
          </FormField>
        </div>
        <div className="mt-2 flex items-center gap-2">
          <span
            className={`w-2 h-2 rounded-full ${config.translate_enabled ? 'bg-green-500' : 'bg-text-tertiary'}`}
          />
          <span className="text-[11px] text-text-tertiary">
            {config.translate_enabled
              ? t('settings.translateOn')
              : t('settings.translateOff')}
          </span>
        </div>
      </Section>

      <Section title={t('settings.agentHotkey')}>
        <p className="text-[11px] text-text-tertiary mb-2">
          {t('settings.agentHotkeyDesc')}
        </p>
        <HotkeyRecorder kind="agent" />
      </Section>

      <Section title={t('settings.recordingBehavior')}>
        <div className="space-y-3">
          <Toggle
            checked={config.auto_pause_media}
            onChange={(checked) => updateConfig({ auto_pause_media: checked })}
            label={t('settings.autoPauseMedia')}
          />
          <p className="text-[11px] text-text-tertiary -mt-1.5 ml-[52px]">
            {t('settings.autoPauseMediaHint')}
          </p>
          <Toggle
            checked={config.agent_notification}
            onChange={(checked) => updateConfig({ agent_notification: checked })}
            label={t('settings.agentNotification')}
          />
          <p className="text-[11px] text-text-tertiary -mt-1.5 ml-[52px]">
            {t('settings.agentNotificationHint')}
          </p>
        </div>
      </Section>

      <Section title={t('settings.outputMode')}>
        <SegmentedControl
          options={[
            { value: 'keyboard', label: t('settings.keyboardSimulation') },
            { value: 'clipboard', label: t('settings.clipboardPaste') },
          ]}
          value={config.output_mode}
          onChange={(v) => updateConfig({ output_mode: v as OutputMode })}
        />
      </Section>

      {isMac && config.output_mode === 'keyboard' && a11yTrusted !== null && (
        <Section title={t('settings.accessibilityPermission')}>
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <span
                className={`w-2 h-2 rounded-full ${a11yTrusted ? 'bg-green-500' : 'bg-amber-500'}`}
              />
              <span className="text-[13px] text-text-primary">
                {a11yTrusted
                  ? t('settings.accessibilityGranted')
                  : t('settings.accessibilityRequired')}
              </span>
            </div>
            {!a11yTrusted && (
              <button
                onClick={handleGrantPermission}
                className="px-3 py-1.5 text-[12px] font-medium text-white bg-accent rounded-full border-none cursor-pointer hover:bg-accent-hover transition-colors"
              >
                {t('settings.grantPermission')}
              </button>
            )}
          </div>
        </Section>
      )}

      <Section title={t('settings.maxRecordingDuration', 'Max Recording Duration')}>
        <div className="flex items-center gap-3">
          <input
            type="range"
            min={10}
            max={300}
            step={10}
            value={config.max_recording_seconds}
            onChange={(e) => updateConfig({ max_recording_seconds: Number(e.target.value) })}
            className="flex-1 accent-accent"
          />
          <span className="text-[13px] text-text-secondary font-mono w-12 text-right">
            {config.max_recording_seconds}s
          </span>
        </div>
      </Section>

      <Section title={t('settings.other')}>
        <div className="space-y-3">
          <Toggle
            checked={config.auto_start}
            onChange={(checked) => updateConfig({ auto_start: checked })}
            label={t('settings.launchAtStartup')}
          />
          {config.auto_start && (
            <Toggle
              checked={config.start_minimized}
              onChange={(checked) => updateConfig({ start_minimized: checked })}
              label={t('settings.startMinimized')}
            />
          )}
          <Toggle
            checked={config.capsule_auto_hide}
            onChange={(checked) => updateConfig({ capsule_auto_hide: checked })}
            label={t('settings.hideCapsuleWhenIdle')}
          />
        </div>
      </Section>
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
