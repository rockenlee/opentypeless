import { useState, useMemo, useEffect, useRef } from 'react'
import { AnimatePresence, motion } from 'framer-motion'
import { useTranslation } from 'react-i18next'
import { Search, Copy, Trash2, Bot } from 'lucide-react'
import { spring } from '../../lib/animations'
import { useAppStore } from '../../stores/appStore'
import { clearHistory, deleteHistoryEntry } from '../../lib/tauri'
import { toast } from '../Toast'

export function History() {
  const history = useAppStore((s) => s.history)
  const setHistory = useAppStore((s) => s.setHistory)
  const setAgentResult = useAppStore((s) => s.setAgentResult)
  const { t } = useTranslation()
  const [search, setSearch] = useState('')
  const [copiedId, setCopiedId] = useState<number | null>(null)
  // Two-stage confirm state. `window.confirm()` is blocked by Tauri 2's
  // webview (always returns false), so we replace it with an inline
  // "click twice to confirm" pattern. `confirmClearArmed` = true after the
  // first click; resets after 3s of inactivity. Same idea for per-row delete.
  const [confirmClearArmed, setConfirmClearArmed] = useState(false)
  const [confirmDeleteId, setConfirmDeleteId] = useState<number | null>(null)
  const clearResetTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const deleteResetTimer = useRef<ReturnType<typeof setTimeout> | null>(null)

  useEffect(() => {
    return () => {
      if (clearResetTimer.current) clearTimeout(clearResetTimer.current)
      if (deleteResetTimer.current) clearTimeout(deleteResetTimer.current)
    }
  }, [])

  const filtered = useMemo(
    () =>
      search
        ? history.filter(
            (h) =>
              h.polished_text.includes(search) ||
              h.raw_text.includes(search) ||
              h.app_name.includes(search),
          )
        : history,
    [history, search],
  )

  const handleCopy = (id: number, text: string) => {
    navigator.clipboard
      .writeText(text)
      .then(() => {
        setCopiedId(id)
        setTimeout(() => setCopiedId(null), 1500)
      })
      .catch(() => {
        toast.error(t('history.failedToCopy'))
      })
  }

  const handleClear = async () => {
    if (!confirmClearArmed) {
      // First click: arm the confirm state. Button label becomes
      // "Click again to clear all" and resets after 3 seconds.
      setConfirmClearArmed(true)
      if (clearResetTimer.current) clearTimeout(clearResetTimer.current)
      clearResetTimer.current = setTimeout(() => setConfirmClearArmed(false), 3000)
      return
    }
    setConfirmClearArmed(false)
    if (clearResetTimer.current) clearTimeout(clearResetTimer.current)
    try {
      await clearHistory()
      setHistory([])
      toast.success(t('history.clearedAll', { defaultValue: 'History cleared' }))
    } catch (e) {
      console.error('Failed to clear history:', e)
      toast.error(t('history.failedToClear'))
    }
  }

  const handleDelete = async (id: number) => {
    if (confirmDeleteId !== id) {
      setConfirmDeleteId(id)
      if (deleteResetTimer.current) clearTimeout(deleteResetTimer.current)
      deleteResetTimer.current = setTimeout(() => setConfirmDeleteId(null), 3000)
      return
    }
    setConfirmDeleteId(null)
    if (deleteResetTimer.current) clearTimeout(deleteResetTimer.current)
    try {
      await deleteHistoryEntry(id)
      setHistory(history.filter((h) => h.id !== id))
    } catch (e) {
      console.error('Failed to delete history entry:', e)
      toast.error(t('history.failedToDelete', { defaultValue: 'Failed to delete' }))
    }
  }

  // Group by date
  const grouped = useMemo(() => {
    const map = new Map<string, typeof filtered>()
    for (const entry of filtered) {
      const date = entry.created_at.split('T')[0] || entry.created_at.split(' ')[0]
      const today = new Date().toISOString().split('T')[0]
      const yesterday = new Date(Date.now() - 86400000).toISOString().split('T')[0]
      const label =
        date === today ? t('history.today') : date === yesterday ? t('history.yesterday') : date
      if (!map.has(label)) map.set(label, [])
      map.get(label)!.push(entry)
    }
    return map
  }, [filtered, t])

  return (
    <div className="w-full h-full bg-bg-primary text-text-primary flex flex-col">
      {/* Header */}
      <div className="flex items-center justify-between px-5 pt-4 pb-3 border-b border-border">
        <h2 className="text-[15px] font-medium">{t('history.title')}</h2>
      </div>

      {/* Search — jelly focus */}
      <div className="px-5 py-3">
        <div className="relative">
          <Search
            size={14}
            className="absolute left-3 top-1/2 -translate-y-1/2 text-text-tertiary"
          />
          <input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder={t('history.searchPlaceholder')}
            className="w-full pl-8 pr-3 py-2.5 bg-bg-secondary border border-border rounded-[14px] text-[13px] text-text-primary outline-none focus:ring-2 focus:ring-jelly-primary focus:border-jelly-primary transition-all jelly-btn"
            style={{ transform: 'none' }}
          />
        </div>
      </div>

      {/* List */}
      <div className="flex-1 overflow-y-auto px-5 pb-4">
        {filtered.length === 0 ? (
          <p className="text-center text-text-tertiary text-[13px] py-12">
            {search ? (
              t('history.noResults')
            ) : (
              <>
                {t('history.noHistory')}
                <br />
                <span className="text-[12px]">{t('history.noHistoryHint')}</span>
              </>
            )}
          </p>
        ) : (
          <AnimatePresence>
            {Array.from(grouped.entries()).map(([label, entries]) => (
              <div key={label} className="mb-4">
                <h3 className="text-[11px] font-medium text-text-tertiary uppercase tracking-wider mb-2 px-1 pb-1 border-b border-border">
                  {label}
                </h3>
                <div className="space-y-0.5">
                  {entries.map((entry) => (
                    <motion.div
                      key={entry.id}
                      whileHover={{ scale: 1.01 }}
                      transition={spring.jellyGentle}
                      className="group flex items-start gap-3 px-3 py-2.5 rounded-[10px] hover:bg-bg-secondary transition-colors"
                    >
                      <div className="flex-1 min-w-0">
                        <p className="text-[13px] text-text-primary leading-relaxed">
                          {entry.polished_text || entry.raw_text}
                        </p>
                        <div className="flex items-center gap-1.5 mt-1">
                          <span className="text-[11px] text-text-tertiary">
                            {entry.created_at.split('T')[1]?.slice(0, 5) || ''} · {entry.app_name}
                          </span>
                          {entry.agent_response && (
                            <span className="inline-flex items-center gap-0.5 text-[10px] text-accent font-medium">
                              <Bot size={10} />
                              Agent
                            </span>
                          )}
                        </div>
                      </div>
                      <div className="flex items-center gap-1 opacity-0 group-hover:opacity-100 transition-all duration-200">
                        {entry.agent_response && (
                          <motion.button
                            onClick={() => setAgentResult(entry.agent_response!)}
                            whileTap={{ scaleX: 1.1, scaleY: 0.9 }}
                            transition={spring.jelly}
                            className="p-1.5 rounded-[6px] hover:bg-bg-tertiary transition-colors bg-transparent border-none cursor-pointer text-accent flex-shrink-0 text-[11px] font-medium"
                            aria-label={t('history.viewAgentDetail')}
                          >
                            {t('history.detail')}
                          </motion.button>
                        )}
                        <motion.button
                          onClick={() => handleCopy(entry.id, entry.agent_response ?? entry.polished_text)}
                          whileTap={{ scaleX: 1.1, scaleY: 0.9 }}
                          transition={spring.jelly}
                          className="p-1.5 rounded-[6px] hover:bg-bg-tertiary transition-all duration-200 bg-transparent border-none cursor-pointer text-text-tertiary hover:text-accent flex-shrink-0"
                          aria-label={`Copy text: ${entry.polished_text.slice(0, 30)}`}
                        >
                          <Copy size={13} />
                        </motion.button>
                        <motion.button
                          onClick={() => handleDelete(entry.id)}
                          whileTap={{ scaleX: 1.1, scaleY: 0.9 }}
                          transition={spring.jelly}
                          className={`p-1.5 rounded-[6px] hover:bg-bg-tertiary transition-all duration-200 bg-transparent border-none cursor-pointer flex-shrink-0 ${
                            confirmDeleteId === entry.id
                              ? 'text-error'
                              : 'text-text-tertiary hover:text-error'
                          }`}
                          aria-label={
                            confirmDeleteId === entry.id
                              ? t('history.confirmDelete', { defaultValue: 'Click again to delete' })
                              : t('history.deleteEntry', { defaultValue: 'Delete entry' })
                          }
                          title={
                            confirmDeleteId === entry.id
                              ? t('history.confirmDelete', { defaultValue: 'Click again to delete' })
                              : t('history.deleteEntry', { defaultValue: 'Delete entry' })
                          }
                        >
                          <Trash2 size={13} />
                        </motion.button>
                      </div>
                      {copiedId === entry.id && (
                        <span className="text-[11px] text-success flex-shrink-0 self-center">
                          {t('history.copied')}
                        </span>
                      )}
                    </motion.div>
                  ))}
                </div>
              </div>
            ))}
          </AnimatePresence>
        )}
      </div>

      {/* Clear button — jelly. Two-stage confirm because `window.confirm()`
          is disabled in Tauri 2 webviews. First click arms (button turns red
          + label changes); second click within 3 seconds actually clears. */}
      {history.length > 0 && (
        <div className="px-5 py-3 border-t border-border">
          <motion.button
            onClick={handleClear}
            whileHover={{ scale: 1.04 }}
            whileTap={{ scaleX: 1.06, scaleY: 0.94 }}
            transition={spring.jellyGentle}
            className={`flex items-center justify-center gap-1.5 w-full py-2 text-[12px] rounded-[10px] cursor-pointer transition-colors jelly-btn ${
              confirmClearArmed
                ? 'text-error bg-error/10 font-medium'
                : 'text-text-tertiary hover:text-error'
            }`}
          >
            <Trash2 size={12} />
            {confirmClearArmed
              ? t('history.clearConfirm', { defaultValue: 'Click again to confirm — clears ALL history' })
              : t('history.clearAll')}
          </motion.button>
        </div>
      )}
    </div>
  )
}
