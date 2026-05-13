import { useState, useEffect } from 'react'
import { AnimatePresence, motion } from 'framer-motion'
import { useTranslation } from 'react-i18next'
import { useAppStore } from '../../stores/appStore'
import { SettingsSidebar, type PaneId } from './SettingsSidebar'
import { GeneralPane } from './GeneralPane'
import { SttPane } from './SttPane'
import { LlmPane } from './LlmPane'
import { AgentPane } from './AgentPane'
import { DictionaryPane } from './DictionaryPane'
import { ScenesPane } from './ScenesPane'
import { AboutPane } from './AboutPane'
import { DirtyBar, useDirtyConfig } from './shared/DirtyBar'

const paneTitleKeys: Record<PaneId, string> = {
  general: 'settings.general',
  stt: 'settings.speechRecognition',
  llm: 'settings.aiPolish',
  agent: 'settings.agent',
  dictionary: 'settings.dictionary',
  scenes: 'settings.scenes',
  about: 'settings.about',
}

export function Settings() {
  const [activePane, setActivePane] = useState<PaneId>('general')
  const config = useAppStore((s) => s.config)
  const setSavedConfig = useAppStore((s) => s.setSavedConfig)
  const isDirty = useDirtyConfig()
  const { t } = useTranslation()

  // Snapshot config when settings opens
  useEffect(() => {
    setSavedConfig(config)
  }, []) // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <div className="w-full h-full bg-bg-primary text-text-primary flex flex-col">
      <div className="flex-1 flex min-h-0">
        {/* Sidebar */}
        <SettingsSidebar activePane={activePane} onSelect={setActivePane} />

        {/* Content */}
        <div className="flex-1 flex flex-col min-w-0">
          {/* Title bar */}
          <div className="flex items-center justify-between px-6 pt-4 pb-3 border-b border-border bg-bg-primary/50">
            <h2 className="text-[15px] font-medium">{t(paneTitleKeys[activePane])}</h2>
          </div>

          {/* Pane content. Three layout adjustments:
              1. `mode="wait"` — old pane fully exits before new mounts, so
                 the new pane doesn't render-stacked-below the old one during
                 the 100ms fade and end up at the bottom of the viewport.
              2. `max-w-[720px] mx-auto` — cap form width so short panes
                 (stt / llm / agent have only 4-5 fields each) don't look
                 stretched-thin across a wide pane.
              3. `pb-8` — breathing room below the form.
              Vertical empty space below short content is by design (standard
              settings UX) — short forms sit at the top of the scroll area. */}
          <div className="flex-1 overflow-y-auto px-6 py-5 pb-8">
            <AnimatePresence mode="wait">
              <motion.div
                key={activePane}
                className="w-full max-w-[720px] mx-auto"
                initial={{ opacity: 0, y: 6 }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0, y: -6 }}
                transition={{ duration: 0.1, ease: 'easeOut' }}
              >
                {activePane === 'general' && <GeneralPane />}
                {activePane === 'stt' && <SttPane />}
                {activePane === 'llm' && <LlmPane />}
                {activePane === 'agent' && <AgentPane />}
                {activePane === 'dictionary' && <DictionaryPane />}
                {activePane === 'scenes' && <ScenesPane />}
                {activePane === 'about' && <AboutPane />}
              </motion.div>
            </AnimatePresence>
          </div>
        </div>
      </div>

      {/* Dirty bar */}
      <AnimatePresence>{isDirty && <DirtyBar />}</AnimatePresence>
    </div>
  )
}
