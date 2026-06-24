import { useState, useEffect } from 'react'
import { motion } from 'framer-motion'
import {
  Check,
  Keyboard,
  MousePointerClick,
  GripHorizontal,
  MousePointer,
  ShieldAlert,
  ShieldCheck,
} from 'lucide-react'
import { useAppStore } from '../../stores/appStore'
import { checkAccessibilityPermission, requestAccessibilityPermission } from '../../lib/tauri'

export function DoneStep() {
  const config = useAppStore((s) => s.config)
  const isMac =
    typeof navigator !== 'undefined' && navigator.platform.toUpperCase().indexOf('MAC') >= 0
  const [a11yTrusted, setA11yTrusted] = useState<boolean | null>(null)
  const showPermissionCard = isMac && config.output_mode === 'keyboard'

  useEffect(() => {
    if (showPermissionCard) {
      checkAccessibilityPermission().then(setA11yTrusted)
      const onFocus = () => checkAccessibilityPermission().then(setA11yTrusted)
      window.addEventListener('focus', onFocus)
      return () => window.removeEventListener('focus', onFocus)
    }
  }, [showPermissionCard])

  const handleGrant = async () => {
    await requestAccessibilityPermission()
    const trusted = await checkAccessibilityPermission()
    setA11yTrusted(trusted)
  }

  return (
    <div className="flex flex-col items-center gap-5 py-2">
      {/* Success animation */}
      <motion.div
        className="w-16 h-16 rounded-full bg-success/10 flex items-center justify-center"
        initial={{ scale: 0 }}
        animate={{ scale: 1 }}
        transition={{ type: 'spring', stiffness: 500, damping: 20 }}
      >
        <motion.div
          initial={{ opacity: 0, scale: 0 }}
          animate={{ opacity: 1, scale: 1 }}
          transition={{ delay: 0.2, type: 'spring', stiffness: 500, damping: 20 }}
        >
          <Check size={28} className="text-success" />
        </motion.div>
      </motion.div>

      <div className="text-center">
        <h2 className="text-[17px] font-semibold text-text-primary">All Set</h2>
        <p className="text-[13px] text-text-secondary mt-1">The capsule is now on your desktop</p>
      </div>

      {/* Usage tips */}
      <div className="w-full space-y-2">
        <Tip
          icon={Keyboard}
          title={`${config.hotkey_mode === 'hold' ? 'Hold' : 'Press'} ${config.hotkey}`}
          desc={config.hotkey_mode === 'hold' ? 'to talk anywhere' : 'to start/stop recording'}
        />
        <Tip icon={MousePointerClick} title="Click the capsule" desc="to start recording" />
        <Tip icon={GripHorizontal} title="Drag to reposition" desc="place it anywhere on screen" />
        <Tip icon={MousePointer} title="Right-click for menu" desc="settings, history, and more" />
      </div>

      {/* macOS Accessibility permission card */}
      {showPermissionCard && a11yTrusted === false && (
        <div className="w-full px-3 py-2.5 bg-amber-500/10 border border-amber-500/20 rounded-[10px]">
          <div className="flex items-center gap-2 mb-2">
            <ShieldAlert size={14} className="text-amber-500 shrink-0" />
            <span className="text-[12px] font-medium text-text-primary">
              Keyboard output requires Accessibility permission
            </span>
          </div>
          <button
            onClick={handleGrant}
            className="w-full py-1.5 text-[12px] font-medium text-white bg-accent rounded-[8px] border-none cursor-pointer hover:bg-accent-hover transition-colors"
          >
            Grant Permission
          </button>
          <p className="text-[10px] text-text-tertiary mt-1.5 text-center">
            You can also grant this later in Settings → General
          </p>
        </div>
      )}
      {showPermissionCard && a11yTrusted === true && (
        <div className="w-full px-3 py-2.5 bg-green-500/10 border border-green-500/20 rounded-[10px]">
          <div className="flex items-center gap-2">
            <ShieldCheck size={14} className="text-green-500 shrink-0" />
            <span className="text-[12px] font-medium text-green-600">
              Accessibility permission granted
            </span>
          </div>
        </div>
      )}
    </div>
  )
}

function Tip({
  icon: Icon,
  title,
  desc,
}: {
  icon: React.ComponentType<{ size?: number; className?: string }>
  title: string
  desc: string
}) {
  return (
    <div className="flex items-center gap-3 px-3 py-2.5 bg-bg-secondary rounded-[10px]">
      <div className="p-1.5 rounded-[8px] bg-bg-tertiary text-text-tertiary shrink-0">
        <Icon size={14} />
      </div>
      <div>
        <p className="text-[13px] font-medium text-text-primary">{title}</p>
        <p className="text-[11px] text-text-tertiary">{desc}</p>
      </div>
    </div>
  )
}
