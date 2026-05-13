import { useEffect, useState } from 'react'
import i18n from './i18n'
import { useTauriEvents } from './hooks/useTauriEvents'
import { useTheme } from './hooks/useTheme'
import { useAppStore } from './stores/appStore'
import { useAuthStore } from './stores/authStore'
import { useRoute } from './lib/router'
import { loadOnboardingCompleted, getConfig, getHistory, getDictionary, checkAccessibilityPermission } from './lib/tauri'
import { initDeepLinkListener } from './lib/deep-link'
import { Capsule } from './components/Capsule'
import { Settings } from './components/Settings'
import { History } from './components/History'
import { Onboarding } from './components/Onboarding'
import { MainLayout } from './components/MainLayout'
import { HomePage } from './components/HomePage'
import { UpgradePage } from './components/UpgradePage'
import { AccountPage } from './components/AccountPage'
import { ToastContainer } from './components/Toast'
import { AgentResultWindow } from './components/AgentResultWindow'

function CapsuleApp() {
  useTauriEvents()
  useTheme()

  const setConfig = useAppStore((s) => s.setConfig)
  const setPipelineError = useAppStore((s) => s.setPipelineError)

  useEffect(() => {
    // Clear ONLY stale pipelineError on mount — never touch pipelineState
    // or recording state. An earlier version of this effect reset them too,
    // which created a race: if `pipeline:state=recording` fired between
    // capsule webview mount and this effect, we'd clobber it back to idle
    // and the capsule would stay on the small idle dot forever.
    setPipelineError(null)

    // Load config so DurationTimer gets the correct max_recording_seconds
    getConfig()
      .then((config) => {
        setConfig(config)
        // Restore UI language from config
        if (config.ui_language && config.ui_language !== i18n.language) {
          i18n.changeLanguage(config.ui_language)
          localStorage.setItem('ui_language', config.ui_language)
        }
      })
      .catch((e) => {
        console.error('Failed to load config in capsule:', e)
      })
  }, [setConfig, setPipelineError])

  // Window show is handled by useCapsuleResize (setSize → setPosition → show),
  // which works on both Windows and macOS. The previous rAF-based show approach
  // failed on macOS because WKWebView pauses requestAnimationFrame in hidden windows.
  return <Capsule />
}

function MainApp() {
  useTauriEvents()
  useTheme()

  const onboardingCompleted = useAppStore((s) => s.onboardingCompleted)
  const setOnboardingCompleted = useAppStore((s) => s.setOnboardingCompleted)
  const setConfig = useAppStore((s) => s.setConfig)
  const setSavedConfig = useAppStore((s) => s.setSavedConfig)
  const setHistory = useAppStore((s) => s.setHistory)
  const setDictionary = useAppStore((s) => s.setDictionary)
  const setAccessibilityTrusted = useAppStore((s) => s.setAccessibilityTrusted)
  const [loaded, setLoaded] = useState(false)
  const [loadError, setLoadError] = useState(false)
  const { route } = useRoute()

  useEffect(() => {
    loadOnboardingCompleted().then(async (done) => {
      setOnboardingCompleted(done)
      if (done) {
        try {
          const [config, history, dictionary] = await Promise.all([
            getConfig(),
            getHistory(200, 0),
            getDictionary(),
          ])
          setConfig(config)
          setSavedConfig(config)
          setHistory(history)
          setDictionary(dictionary)
          // Check macOS Accessibility permission
          if (navigator.platform.toUpperCase().indexOf('MAC') >= 0) {
            checkAccessibilityPermission().then((trusted) => {
              setAccessibilityTrusted(trusted)
            })
          }
          // Restore UI language from config
          if (config.ui_language && config.ui_language !== i18n.language) {
            i18n.changeLanguage(config.ui_language)
            localStorage.setItem('ui_language', config.ui_language)
          }
        } catch (e) {
          console.error('Failed to load initial data:', e)
          setLoadError(true)
        }
      }
      setLoaded(true)
    })

    // Initialize auth session (non-blocking)
    useAuthStore.getState().initialize()

    // Initialize deep-link listener
    initDeepLinkListener()
  }, [setOnboardingCompleted, setConfig, setSavedConfig, setHistory, setDictionary, setAccessibilityTrusted])

  const user = useAuthStore((s) => s.user)

  // Periodically refresh subscription status + refresh on window focus (throttled)
  useEffect(() => {
    if (!loaded || !user) return

    let lastRefresh = 0
    const throttledRefresh = () => {
      const now = Date.now()
      const { checkoutPending } = useAuthStore.getState()
      // Skip throttle if user just came back from checkout
      if (!checkoutPending && now - lastRefresh < 30_000) return
      lastRefresh = now
      useAuthStore.getState().refreshSubscription()
    }

    const interval = setInterval(
      () => {
        lastRefresh = Date.now()
        useAuthStore.getState().refreshSubscription()
      },
      5 * 60 * 1000,
    )

    window.addEventListener('focus', throttledRefresh)

    return () => {
      clearInterval(interval)
      window.removeEventListener('focus', throttledRefresh)
    }
  }, [loaded, user])

  if (!loaded)
    return (
      <div className="flex items-center justify-center h-screen">
        <span className="text-text-tertiary text-[13px]">Loading...</span>
      </div>
    )
  if (loadError)
    return (
      <div className="flex flex-col items-center justify-center h-screen gap-3">
        <span className="text-error text-[13px]">Failed to load application data.</span>
        <button
          onClick={() => window.location.reload()}
          className="px-4 py-2 bg-accent text-white rounded-[10px] text-[13px] border-none cursor-pointer hover:bg-accent-hover transition-colors"
        >
          Retry
        </button>
      </div>
    )
  if (!onboardingCompleted) return <Onboarding />

  return (
    <MainLayout>
      {route === 'home' && <HomePage />}
      {route === 'settings' && <Settings />}
      {route === 'history' && <History />}
      {route === 'upgrade' && <UpgradePage />}
      {route === 'account' && <AccountPage />}
      <ToastContainer />
    </MainLayout>
  )
}

// Tauri 2 injects `__TAURI_INTERNALS__.metadata.currentWindow.label` before any user JS runs,
// so we can identify the window synchronously without relying on hash routing — which proved
// fragile because navigation in the main window mutates `location.hash`, and because
// Tauri webview navigation can race the hash being available when React first renders.
function currentWindowLabel(): string | null {
  const label = (
    window as unknown as {
      __TAURI_INTERNALS__?: { metadata?: { currentWindow?: { label?: string } } }
    }
  ).__TAURI_INTERNALS__?.metadata?.currentWindow?.label
  return label ?? null
}

function App() {
  const label = currentWindowLabel()
  // Prefer the Tauri window label; fall back to URL hash for non-Tauri contexts (tests).
  if (label === 'capsule' || window.location.hash === '#capsule') return <CapsuleApp />
  if (label === 'agent-result' || window.location.hash === '#agent-result')
    return <AgentResultWindow />
  return <MainApp />
}

export default App
