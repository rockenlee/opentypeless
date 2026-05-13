import { Settings, Mic, Sparkles, BookOpen, Info, LayoutGrid, Bot } from 'lucide-react'
import { motion } from 'framer-motion'
import { useTranslation } from 'react-i18next'
import { spring } from '../../lib/animations'

const PANES = [
  { id: 'general', labelKey: 'settings.general', icon: Settings },
  { id: 'stt', labelKey: 'settings.speechRecognition', icon: Mic },
  { id: 'llm', labelKey: 'settings.aiPolish', icon: Sparkles },
  { id: 'agent', labelKey: 'settings.agent', icon: Bot },
  { id: 'dictionary', labelKey: 'settings.dictionary', icon: BookOpen },
  { id: 'scenes', labelKey: 'settings.scenes', icon: LayoutGrid },
  { id: 'about', labelKey: 'settings.about', icon: Info },
] as const

export type PaneId = (typeof PANES)[number]['id']

interface Props {
  activePane: PaneId
  onSelect: (id: PaneId) => void
}

export function SettingsSidebar({ activePane, onSelect }: Props) {
  const { t } = useTranslation()

  return (
    <div className="w-[200px] h-full jelly-surface-flat border-r border-border flex flex-col py-4 px-2 gap-0.5">
      <h2 className="text-[13px] font-semibold text-text-primary px-3 pb-3">
        {t('settings.title')}
      </h2>
      {PANES.map((pane) => {
        const Icon = pane.icon
        const isActive = activePane === pane.id
        return (
          <motion.button
            key={pane.id}
            onClick={() => onSelect(pane.id)}
            whileHover={{ scale: 1.03 }}
            whileTap={{ scaleX: 1.05, scaleY: 0.95 }}
            transition={spring.jellyGentle}
            className={`flex items-center gap-2.5 w-full px-3 py-2 rounded-[8px] text-[13px] border-none cursor-pointer transition-colors text-left relative ${
              isActive
                ? 'text-text-primary font-medium'
                : 'bg-transparent text-text-secondary hover:text-text-primary'
            }`}
          >
            {isActive && (
              <motion.div
                layoutId="settings-nav-indicator"
                className="absolute inset-0 jelly-nav-active"
                transition={spring.jellyGentle}
              />
            )}
            <span className="relative z-10 flex items-center gap-2.5">
              <Icon size={16} />
              {t(pane.labelKey)}
            </span>
          </motion.button>
        )
      })}
    </div>
  )
}
