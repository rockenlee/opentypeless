import { useState, useCallback } from 'react'
import { useTranslation } from 'react-i18next'
import { X } from 'lucide-react'
import { useAppStore, type DictionaryEntry } from '../../stores/appStore'
import { bulkAddDictionaryEntries, getDictionary } from '../../lib/tauri'
import { toast } from '../Toast'

interface ParsedLine {
  line: number
  word: string
  pronunciation: string | null
  status: 'valid' | 'duplicate_existing' | 'duplicate_internal' | 'invalid'
  reason?: string
}

interface PreviewResult {
  valid: ParsedLine[]
  duplicateExisting: ParsedLine[]
  duplicateInternal: ParsedLine[]
  invalid: ParsedLine[]
}

function stripBom(text: string): string {
  return text.charCodeAt(0) === 0xfeff ? text.slice(1) : text
}

/** Detect if the first non-empty line is a known header row. */
function isHeaderRow(line: string): boolean {
  const lower = line.toLowerCase().replace(/\s+/g, '')
  return lower === 'word,pronunciation' || lower === 'word\tpronunciation'
}

function parseCsvField(raw: string): { fields: string[]; error?: string } {
  const fields: string[] = []
  let i = 0
  while (i < raw.length) {
    if (raw[i] === '"') {
      // Quoted field
      let value = ''
      i++ // skip opening quote
      let closed = false
      while (i < raw.length) {
        if (raw[i] === '"') {
          if (i + 1 < raw.length && raw[i + 1] === '"') {
            value += '"'
            i += 2
          } else {
            i++ // skip closing quote
            closed = true
            break
          }
        } else {
          value += raw[i]
          i++
        }
      }
      if (!closed) {
        return { fields: [], error: 'csvFormatError' }
      }
      // After closing quote, must be comma or end of string
      if (i < raw.length && raw[i] !== ',') {
        return { fields: [], error: 'csvFormatError' }
      }
      fields.push(value)
      if (i < raw.length && raw[i] === ',') i++
    } else {
      const comma = raw.indexOf(',', i)
      if (comma === -1) {
        fields.push(raw.slice(i))
        break
      } else {
        fields.push(raw.slice(i, comma))
        i = comma + 1
      }
    }
  }
  return { fields }
}

function parseLine(line: string): { word: string; pronunciation: string | null; error?: string } {
  const trimmed = line.trim()

  // Tab-separated takes priority
  if (trimmed.includes('\t')) {
    const parts = trimmed.split('\t')
    if (parts.length > 2) {
      return { word: '', pronunciation: null, error: 'tooManyColumns' }
    }
    const word = (parts[0] ?? '').trim()
    const pron = parts.length === 2 ? (parts[1] ?? '').trim() || null : null
    return { word, pronunciation: pron }
  }

  // CSV (comma-separated)
  if (trimmed.includes(',') || trimmed.includes('"')) {
    const result = parseCsvField(trimmed)
    if (result.error) {
      return { word: '', pronunciation: null, error: result.error }
    }
    const { fields } = result
    if (fields.length > 2) {
      return { word: '', pronunciation: null, error: 'tooManyColumns' }
    }
    const word = (fields[0] ?? '').trim()
    const pron = fields.length === 2 ? (fields[1] ?? '').trim() || null : null
    return { word, pronunciation: pron }
  }

  // Single column
  return { word: trimmed, pronunciation: null }
}

function buildPreview(text: string, existingWords: Set<string>): PreviewResult {
  const cleaned = stripBom(text)
  const lines = cleaned.split(/\r?\n/)

  const result: PreviewResult = {
    valid: [],
    duplicateExisting: [],
    duplicateInternal: [],
    invalid: [],
  }

  const seenWords = new Map<string, number>() // word -> first line number

  let startIdx = 0
  // Skip header row if present
  const firstNonEmpty = lines.findIndex((l) => l.trim().length > 0)
  if (firstNonEmpty >= 0 && isHeaderRow(lines[firstNonEmpty])) {
    startIdx = firstNonEmpty + 1
  }

  for (let i = startIdx; i < lines.length; i++) {
    const raw = lines[i]
    if (raw.trim().length === 0) continue

    const lineNum = i + 1
    const { word, pronunciation, error } = parseLine(raw)

    if (error) {
      result.invalid.push({
        line: lineNum,
        word: raw.trim(),
        pronunciation: null,
        status: 'invalid',
        reason: error,
      })
      continue
    }

    if (word.length === 0) {
      result.invalid.push({
        line: lineNum,
        word: raw.trim(),
        pronunciation: null,
        status: 'invalid',
        reason: 'emptyWord',
      })
      continue
    }

    if ([...word].length > 100) {
      result.invalid.push({
        line: lineNum,
        word,
        pronunciation,
        status: 'invalid',
        reason: 'wordTooLong',
      })
      continue
    }

    if (pronunciation && [...pronunciation].length > 100) {
      result.invalid.push({
        line: lineNum,
        word,
        pronunciation,
        status: 'invalid',
        reason: 'pronunciationTooLong',
      })
      continue
    }

    // Check existing dictionary
    if (existingWords.has(word)) {
      result.duplicateExisting.push({
        line: lineNum,
        word,
        pronunciation,
        status: 'duplicate_existing',
      })
      continue
    }

    // Check internal duplicate
    const firstSeen = seenWords.get(word)
    if (firstSeen !== undefined) {
      result.duplicateInternal.push({
        line: lineNum,
        word,
        pronunciation,
        status: 'duplicate_internal',
        reason: String(firstSeen),
      })
      continue
    }

    seenWords.set(word, lineNum)
    result.valid.push({
      line: lineNum,
      word,
      pronunciation,
      status: 'valid',
    })
  }

  return result
}

export function BulkImportDialog({ onClose }: { onClose: () => void }) {
  const dictionary = useAppStore((s) => s.dictionary) as DictionaryEntry[]
  const setDictionary = useAppStore((s) => s.setDictionary)
  const { t } = useTranslation()

  const [text, setText] = useState('')
  const [preview, setPreview] = useState<PreviewResult | null>(null)
  const [importing, setImporting] = useState(false)
  const [done, setDone] = useState(false)
  const [importedCount, setImportedCount] = useState(0)
  const [expandSection, setExpandSection] = useState<string | null>(null)

  const handlePreview = useCallback(() => {
    const existing = new Set(dictionary.map((e) => e.word))
    const result = buildPreview(text, existing)
    setPreview(result)
    setExpandSection(null)
  }, [text, dictionary])

  const handleImport = useCallback(async () => {
    if (!preview || preview.valid.length === 0) return
    setImporting(true)
    try {
      const entries: [string, string | null][] = preview.valid.map((p) => [
        p.word,
        p.pronunciation,
      ])
      const result = await bulkAddDictionaryEntries(entries)
      setImportedCount(result.imported)
      setDone(true)
      // Refresh store
      const updated = await getDictionary()
      setDictionary(updated)
    } catch (e) {
      console.error('Bulk import failed:', e)
      toast.error(t('dictionary.bulkImport.importFailed'))
    } finally {
      setImporting(false)
    }
  }, [preview, setDictionary, t])

  const reasonLabel = (p: ParsedLine) => {
    switch (p.reason) {
      case 'emptyWord':
        return t('dictionary.bulkImport.errorEmptyWord')
      case 'wordTooLong':
        return t('dictionary.bulkImport.errorWordTooLong')
      case 'pronunciationTooLong':
        return t('dictionary.bulkImport.errorPronTooLong')
      case 'tooManyColumns':
        return t('dictionary.bulkImport.errorTooManyColumns')
      case 'csvFormatError':
        return t('dictionary.bulkImport.errorCsvFormat')
      default:
        if (p.status === 'duplicate_existing') {
          return t('dictionary.bulkImport.reasonDuplicateExisting')
        }
        if (p.status === 'duplicate_internal') {
          return t('dictionary.bulkImport.reasonDuplicateInternal', { line: p.reason })
        }
        return ''
    }
  }

  const renderSection = (
    key: string,
    label: string,
    items: ParsedLine[],
    color: string,
  ) => {
    if (items.length === 0) return null
    const isExpanded = expandSection === key
    return (
      <div className="mt-2">
        <button
          onClick={() => setExpandSection(isExpanded ? null : key)}
          className={`text-[12px] font-medium bg-transparent border-none cursor-pointer px-0 ${color}`}
        >
          {label} ({items.length}) {isExpanded ? '▾' : '▸'}
        </button>
        {isExpanded && (
          <div className="max-h-32 overflow-y-auto border border-border rounded-[8px] mt-1">
            <table className="w-full text-[12px]">
              <tbody>
                {items.map((p) => (
                  <tr key={`${key}-${p.line}`} className="border-b border-border last:border-b-0">
                    <td className="px-2 py-1 text-text-tertiary w-10">#{p.line}</td>
                    <td className="px-2 py-1">{p.word}</td>
                    <td className="px-2 py-1 text-text-tertiary">{reasonLabel(p)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    )
  }

  return (
    <div className="fixed inset-0 z-40 flex items-center justify-center bg-black/40">
      <div
        className="bg-bg-primary border border-border rounded-2xl shadow-2xl w-[520px] max-h-[80vh] flex flex-col overflow-hidden"
        onKeyDown={(e) => {
          if (e.key === 'Escape') onClose()
        }}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-border">
          <span className="text-[14px] font-medium text-text-primary">
            {t('dictionary.bulkImport.title')}
          </span>
          <button
            onClick={onClose}
            className="p-1 rounded-[6px] hover:bg-bg-tertiary transition-colors bg-transparent border-none cursor-pointer text-text-tertiary"
          >
            <X size={16} />
          </button>
        </div>

        {/* Body */}
        <div className="px-4 py-3 flex-1 overflow-y-auto space-y-3">
          {!done ? (
            <>
              <p className="text-[12px] text-text-secondary m-0">
                {t('dictionary.bulkImport.hint')}
              </p>
              <textarea
                value={text}
                onChange={(e) => {
                  setText(e.target.value)
                  setPreview(null)
                }}
                placeholder={t('dictionary.bulkImport.placeholder')}
                className="w-full h-40 px-3 py-2 bg-bg-secondary border border-border rounded-[10px] text-[13px] text-text-primary outline-none focus:border-border-focus transition-colors resize-none font-mono"
              />

              {preview && (
                <div className="space-y-1">
                  <div className="flex gap-3 text-[12px]">
                    <span className="text-green-500">
                      {t('dictionary.bulkImport.validCount', { count: preview.valid.length })}
                    </span>
                    <span className="text-yellow-500">
                      {t('dictionary.bulkImport.duplicateCount', {
                        count: preview.duplicateExisting.length + preview.duplicateInternal.length,
                      })}
                    </span>
                    <span className="text-red-400">
                      {t('dictionary.bulkImport.invalidCount', { count: preview.invalid.length })}
                    </span>
                  </div>
                  {renderSection(
                    'dup-existing',
                    t('dictionary.bulkImport.duplicateExisting'),
                    preview.duplicateExisting,
                    'text-yellow-500',
                  )}
                  {renderSection(
                    'dup-internal',
                    t('dictionary.bulkImport.duplicateInternal'),
                    preview.duplicateInternal,
                    'text-yellow-500',
                  )}
                  {renderSection(
                    'invalid',
                    t('dictionary.bulkImport.invalidEntries'),
                    preview.invalid,
                    'text-red-400',
                  )}
                </div>
              )}
            </>
          ) : (
            <div className="py-4 space-y-2">
              <p className="text-[14px] text-text-primary m-0 text-center">
                {t('dictionary.bulkImport.doneMessage', { count: importedCount })}
              </p>
              {preview && (preview.duplicateExisting.length + preview.duplicateInternal.length > 0) && (
                <p className="text-[12px] text-text-secondary m-0 text-center">
                  {t('dictionary.bulkImport.skippedDuplicates', {
                    count: preview.duplicateExisting.length + preview.duplicateInternal.length,
                  })}
                </p>
              )}
              {preview && preview.invalid.length > 0 && (
                <p className="text-[12px] text-text-secondary m-0 text-center">
                  {t('dictionary.bulkImport.skippedInvalid', { count: preview.invalid.length })}
                </p>
              )}
              {preview && (
                <>
                  {renderSection(
                    'done-dup-existing',
                    t('dictionary.bulkImport.duplicateExisting'),
                    preview.duplicateExisting,
                    'text-yellow-500',
                  )}
                  {renderSection(
                    'done-dup-internal',
                    t('dictionary.bulkImport.duplicateInternal'),
                    preview.duplicateInternal,
                    'text-yellow-500',
                  )}
                  {renderSection(
                    'done-invalid',
                    t('dictionary.bulkImport.invalidEntries'),
                    preview.invalid,
                    'text-red-400',
                  )}
                </>
              )}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex justify-end gap-2 px-4 py-3 border-t border-border">
          {done ? (
            <button
              onClick={onClose}
              className="px-4 py-2 bg-accent text-white rounded-[10px] text-[13px] border-none cursor-pointer hover:bg-accent-hover transition-colors"
            >
              {t('dictionary.bulkImport.close')}
            </button>
          ) : (
            <>
              {!preview ? (
                <button
                  onClick={handlePreview}
                  disabled={!text.trim()}
                  className="px-4 py-2 bg-accent text-white rounded-[10px] text-[13px] border-none cursor-pointer hover:bg-accent-hover disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
                >
                  {t('dictionary.bulkImport.preview')}
                </button>
              ) : (
                <>
                  <button
                    onClick={() => setPreview(null)}
                    className="px-4 py-2 bg-bg-secondary text-text-primary rounded-[10px] text-[13px] border border-border cursor-pointer hover:bg-bg-tertiary transition-colors"
                  >
                    {t('dictionary.bulkImport.back')}
                  </button>
                  <button
                    onClick={handleImport}
                    disabled={preview.valid.length === 0 || importing}
                    className="px-4 py-2 bg-accent text-white rounded-[10px] text-[13px] border-none cursor-pointer hover:bg-accent-hover disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
                  >
                    {importing
                      ? t('dictionary.bulkImport.importing')
                      : t('dictionary.bulkImport.importN', { count: preview.valid.length })}
                  </button>
                </>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  )
}
