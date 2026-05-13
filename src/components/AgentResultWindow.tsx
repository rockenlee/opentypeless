import { useEffect, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { Check, Copy, X } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { getLastAgentResult } from '../lib/tauri'

export function AgentResultWindow() {
  const { t } = useTranslation()
  const [result, setResult] = useState('')
  const [copied, setCopied] = useState(false)

  useEffect(() => {
    const current = getCurrentWindow()
    current.show().catch(() => {})
    current.setFocus().catch(() => {})

    let cancelled = false
    let unlisten: (() => void) | undefined
    getLastAgentResult()
      .then((latest) => {
        if (!cancelled && latest) setResult(latest)
      })
      .catch(() => {})

    listen<string>('agent:result', (event) => {
      if (!cancelled) setResult(event.payload)
    }).then((fn) => {
      if (cancelled) {
        fn()
      } else {
        unlisten = fn
      }
    })

    return () => {
      cancelled = true
      unlisten?.()
    }
  }, [])

  const handleCopy = () => {
    if (!result) return
    navigator.clipboard.writeText(result).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    })
  }

  const handleClose = () => {
    getCurrentWindow()
      .close()
      .catch(() => {})
  }

  return (
    <div className="w-screen h-screen bg-bg-primary text-text-primary flex flex-col">
      <header className="h-12 px-4 border-b border-border flex items-center justify-between">
        <h1 className="text-[13px] font-medium">{t('agent.resultTitle')}</h1>
        <div className="flex items-center gap-1">
          <button
            onClick={handleCopy}
            disabled={!result}
            className="p-1.5 rounded-[6px] hover:bg-bg-secondary transition-colors text-text-tertiary hover:text-text-primary bg-transparent border-none cursor-pointer disabled:opacity-40"
            aria-label={t('agent.copyResult')}
          >
            {copied ? <Check size={14} className="text-success" /> : <Copy size={14} />}
          </button>
          <button
            onClick={handleClose}
            className="p-1.5 rounded-[6px] hover:bg-bg-secondary transition-colors text-text-tertiary hover:text-text-primary bg-transparent border-none cursor-pointer"
            aria-label={t('agent.closePanel')}
          >
            <X size={14} />
          </button>
        </div>
      </header>
      <main className="flex-1 overflow-y-auto p-4">
        {result ? (
          <pre className="text-[13px] text-text-primary leading-relaxed whitespace-pre-wrap break-words font-sans">
            {result}
          </pre>
        ) : (
          <div className="h-full flex items-center justify-center text-[13px] text-text-tertiary">
            {t('agent.waitingForResult')}
          </div>
        )}
      </main>
    </div>
  )
}
