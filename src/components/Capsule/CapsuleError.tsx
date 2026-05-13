import { useEffect } from 'react'
import { motion } from 'framer-motion'
import { useTranslation } from 'react-i18next'
import { useAppStore } from '../../stores/appStore'

export function CapsuleError() {
  const { t } = useTranslation()
  const pipelineError = useAppStore((s) => s.pipelineError)
  const setPipelineError = useAppStore((s) => s.setPipelineError)
  const resetRecording = useAppStore((s) => s.resetRecording)

  useEffect(() => {
    const timer = setTimeout(() => {
      setPipelineError(null)
      // Only reset recording state if the pipeline is actually idle.
      // If the user started a new recording during the 2.5s error window,
      // don't overwrite the active pipeline state.
      const currentState = useAppStore.getState().pipelineState
      if (currentState === 'idle') {
        resetRecording()
      }
    }, 2500)
    return () => clearTimeout(timer)
  }, [setPipelineError, resetRecording, pipelineError])

  return (
    <motion.div
      className="relative z-10 flex items-start gap-2 min-h-9 px-3 py-1"
      initial={{ opacity: 0, x: -4 }}
      animate={{ opacity: 1, x: 0 }}
      transition={{ duration: 0.3, ease: 'easeOut' }}
    >
      {/* White dot */}
      <motion.div className="w-2 h-2 rounded-full bg-white/80 flex-shrink-0" />
      <p className="text-[11px] text-white break-all flex-1 whitespace-pre-wrap">
        {pipelineError === 'ACCESSIBILITY_REQUIRED'
          ? t('capsule.accessibilityRequired')
          : pipelineError || 'An error occurred'}
      </p>
    </motion.div>
  )
}
