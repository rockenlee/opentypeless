import { useEffect, useRef } from 'react'
import { motion, AnimatePresence } from 'framer-motion'
import { X, Copy, Check } from 'lucide-react'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { spring } from '../lib/animations'
import { useAppStore } from '../stores/appStore'

export function AgentResultPanel() {
  const { t } = useTranslation()
  const agentResult = useAppStore((s) => s.agentResult)
  const setAgentResult = useAppStore((s) => s.setAgentResult)
  const [copied, setCopied] = useState(false)
  const preRef = useRef<HTMLPreElement>(null)

  // Close on Escape
  useEffect(() => {
    if (!agentResult) return
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setAgentResult(null)
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [agentResult, setAgentResult])

  const handleCopy = () => {
    if (!agentResult) return
    navigator.clipboard.writeText(agentResult).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    })
  }

  return (
    <AnimatePresence>
      {agentResult && (
        <>
          {/* Backdrop */}
          <motion.div
            className="fixed inset-0 z-40 bg-black/40"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.2 }}
            onClick={() => setAgentResult(null)}
          />

          {/* Panel */}
          <motion.div
            className="fixed inset-x-4 bottom-4 top-16 z-50 flex flex-col bg-bg-primary border border-border rounded-2xl shadow-2xl overflow-hidden"
            initial={{ opacity: 0, y: 24, scale: 0.97 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: 16, scale: 0.97 }}
            transition={spring.jellyGentle}
          >
            {/* Header */}
            <div className="flex items-center justify-between px-4 py-3 border-b border-border flex-shrink-0">
              <span className="text-[13px] font-medium text-text-primary">
                {t('agent.resultTitle')}
              </span>
              <div className="flex items-center gap-1">
                <motion.button
                  onClick={handleCopy}
                  whileTap={{ scale: 0.9 }}
                  transition={spring.jelly}
                  className="p-1.5 rounded-[6px] hover:bg-bg-secondary transition-colors text-text-tertiary hover:text-text-primary bg-transparent border-none cursor-pointer"
                  aria-label={t('agent.copyResult')}
                >
                  {copied ? <Check size={14} className="text-success" /> : <Copy size={14} />}
                </motion.button>
                <motion.button
                  onClick={() => setAgentResult(null)}
                  whileTap={{ scale: 0.9 }}
                  transition={spring.jelly}
                  className="p-1.5 rounded-[6px] hover:bg-bg-secondary transition-colors text-text-tertiary hover:text-text-primary bg-transparent border-none cursor-pointer"
                  aria-label={t('agent.closePanel')}
                >
                  <X size={14} />
                </motion.button>
              </div>
            </div>

            {/* Content */}
            <div className="flex-1 overflow-y-auto p-4">
              <pre
                ref={preRef}
                className="text-[13px] text-text-primary leading-relaxed whitespace-pre-wrap break-words font-sans"
              >
                {agentResult}
              </pre>
            </div>
          </motion.div>
        </>
      )}
    </AnimatePresence>
  )
}
