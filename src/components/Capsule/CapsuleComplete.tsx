import { motion } from 'framer-motion'
import { Check, Loader2 } from 'lucide-react'
import { useEffect } from 'react'
import { useTranslation } from 'react-i18next'
import { useAppStore } from '../../stores/appStore'
import { spring } from '../../lib/animations'

export function CapsuleComplete() {
  const resetRecording = useAppStore((s) => s.resetRecording)
  const setPipelineState = useAppStore((s) => s.setPipelineState)
  const editSelection = useAppStore((s) => s.editSelection)
  const { t } = useTranslation()

  useEffect(() => {
    const timer = setTimeout(() => {
      resetRecording()
      setPipelineState('idle')
    }, 1200)
    return () => clearTimeout(timer)
  }, [resetRecording, setPipelineState])

  // While in Edit Selection the "outputting" state means we're replacing the
  // selection — show a neutral "replacing" label, not a success check. The real
  // success/failure is confirmed by the localized toast + capsule error surface.
  if (editSelection.active) {
    return (
      <motion.div className="relative z-10 flex items-center gap-1.5 h-9 px-3">
        <Loader2 size={12} className="text-white/80 animate-spin" />
        <span className="text-[11px] text-white font-medium whitespace-nowrap">
          {t('editSelection.replacing')}
        </span>
      </motion.div>
    )
  }

  return (
    <motion.div className="relative z-10 flex items-center gap-1.5 h-9 px-3">
      <motion.div
        initial={{ scale: 0, opacity: 0 }}
        animate={{ scale: 1, opacity: 1 }}
        transition={spring.smooth}
      >
        <Check size={14} className="text-white" />
      </motion.div>
      <span className="text-[11px] text-white font-medium">Done</span>
    </motion.div>
  )
}
