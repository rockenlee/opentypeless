import { motion, AnimatePresence } from 'framer-motion'
import { Sparkles } from 'lucide-react'
import { openUrl } from '@tauri-apps/plugin-opener'
import { useAppStore } from '../../stores/appStore'

// Shown when the backend's startup update check finds a newer GitHub release.
// Purely a reminder — "下载" opens the release page; the user updates manually.
export function UpdateBanner() {
  const updateInfo = useAppStore((s) => s.updateInfo)
  const setUpdateInfo = useAppStore((s) => s.setUpdateInfo)

  return (
    <AnimatePresence>
      {updateInfo && (
        <motion.div
          initial={{ height: 0, opacity: 0 }}
          animate={{ height: 'auto', opacity: 1 }}
          exit={{ height: 0, opacity: 0 }}
          transition={{ duration: 0.2 }}
          className="overflow-hidden"
        >
          <div className="flex items-center gap-2 px-4 py-2 bg-accent/10 border-b border-accent/20">
            <Sparkles size={14} className="text-accent shrink-0" />
            <span className="text-[12px] text-text-primary flex-1">
              新版本 {updateInfo.version} 可用
            </span>
            <button
              onClick={() => openUrl(updateInfo.url)}
              className="px-3 py-1 text-[11px] font-medium text-white bg-accent rounded-full border-none cursor-pointer hover:bg-accent-hover transition-colors shrink-0"
            >
              下载
            </button>
            <button
              onClick={() => setUpdateInfo(null)}
              className="text-text-tertiary text-[12px] border-none bg-transparent cursor-pointer hover:text-text-secondary shrink-0"
              aria-label="dismiss"
            >
              ✕
            </button>
          </div>
        </motion.div>
      )}
    </AnimatePresence>
  )
}
