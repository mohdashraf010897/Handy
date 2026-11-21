import React, {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { readFile } from "@tauri-apps/plugin-fs";
import {
  BookPlus,
  Check,
  Copy,
  FileAudio,
  FilePlus,
  FolderOpen,
  RotateCcw,
  Star,
  Trash2,
  X,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import {
  commands,
  events,
  type HistoryEntry,
  type HistoryUpdatePayload,
} from "@/bindings";
import { useOsType } from "@/hooks/useOsType";
import { useSettingsStore } from "@/stores/settingsStore";
import {
  useTranscriptionJobsStore,
  type ActiveJobStatus,
} from "@/stores/transcriptionJobsStore";
import { formatDateTime } from "@/utils/dateFormat";
import { AudioPlayer, AudioPlayerGroup } from "../../ui/AudioPlayer";
import { Button } from "../../ui/Button";
import { Input } from "../../ui/Input";
import { copyToClipboard } from "./clipboard";
import { pickAndTranscribeFiles } from "./transcribeFiles";

function normalizeDictionaryCandidate(raw: string): string {
  return raw
    .replace(/[<>"']/g, "")
    .trim()
    .replace(/\s+/g, " ")
    .slice(0, 50);
}

function extractDiffTokens(text: string): string[] {
  return text.match(/[\p{L}\p{N}]+(?:[-'&][\p{L}\p{N}]+)*/gu) ?? [];
}

function suggestDictionaryWord(
  originalText: string,
  editedText: string,
): string {
  const orig = extractDiffTokens(originalText);
  const next = extractDiffTokens(editedText);

  let prefixLen = 0;
  while (
    prefixLen < orig.length &&
    prefixLen < next.length &&
    orig[prefixLen] === next[prefixLen]
  ) {
    prefixLen += 1;
  }

  let suffixLen = 0;
  while (
    suffixLen < orig.length - prefixLen &&
    suffixLen < next.length - prefixLen &&
    orig[orig.length - 1 - suffixLen] === next[next.length - 1 - suffixLen]
  ) {
    suffixLen += 1;
  }

  const added = next.slice(prefixLen, next.length - suffixLen);
  if (added.length === 0 || added.length > 3) {
    return "";
  }

  const candidate = normalizeDictionaryCandidate(added.join(" "));
  return candidate.length <= 50 ? candidate : "";
}

const IconButton: React.FC<{
  onClick: () => void;
  onMouseDown?: () => void;
  title: string;
  disabled?: boolean;
  active?: boolean;
  children: React.ReactNode;
}> = ({ onClick, onMouseDown, title, disabled, active, children }) => (
  <button
    type="button"
    onMouseDown={onMouseDown}
    onClick={onClick}
    disabled={disabled}
    aria-label={title}
    className={`p-1.5 rounded-md flex items-center justify-center transition-colors cursor-pointer disabled:cursor-not-allowed disabled:text-text/20 ${
      active
        ? "text-logo-primary hover:text-logo-primary"
        : "text-ink-3 hover:text-logo-primary"
    }`}
    title={title}
  >
    {children}
  </button>
);

const PAGE_SIZE = 30;

// Long text folds to this many lines until "Show more".
const FOLDED_LINES = 4;

interface HeaderButtonProps {
  onClick: () => void;
  label: string;
  icon: React.ComponentType<{ className?: string }>;
}

const HeaderButton: React.FC<HeaderButtonProps> = ({
  onClick,
  label,
  icon: Icon,
}) => (
  <Button
    onClick={onClick}
    variant="secondary"
    size="sm"
    className="flex items-center gap-2"
    title={label}
  >
    <Icon className="w-4 h-4" />
    <span>{label}</span>
  </Button>
);

export const HistorySettings: React.FC = () => {
  const { t } = useTranslation();
  const osType = useOsType();
  const jobs = useTranscriptionJobsStore((state) => state.jobs);
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [hasMore, setHasMore] = useState(true);
  const sentinelRef = useRef<HTMLDivElement>(null);
  const entriesRef = useRef<HistoryEntry[]>([]);
  const loadingRef = useRef(false);

  // Keep ref in sync for use in IntersectionObserver callback
  useEffect(() => {
    entriesRef.current = entries;
  }, [entries]);

  const loadPage = useCallback(async (cursor?: number) => {
    const isFirstPage = cursor === undefined;
    if (!isFirstPage && loadingRef.current) return;
    loadingRef.current = true;

    if (isFirstPage) setLoading(true);

    try {
      const result = await commands.getHistoryEntries(
        cursor ?? null,
        PAGE_SIZE,
      );
      if (result.status === "ok") {
        const { entries: newEntries, has_more } = result.data;
        setEntries((prev) => {
          if (isFirstPage) return newEntries;
          const existingIds = new Set(prev.map((entry) => entry.id));
          return [
            ...prev,
            ...newEntries.filter((entry) => !existingIds.has(entry.id)),
          ];
        });
        setHasMore(has_more);
      }
    } catch (error) {
      console.error("Failed to load history entries:", error);
    } finally {
      setLoading(false);
      loadingRef.current = false;
    }
  }, []);

  // Initial load
  useEffect(() => {
    loadPage();
  }, [loadPage]);

  // Infinite scroll via IntersectionObserver
  useEffect(() => {
    if (loading) return;

    const sentinel = sentinelRef.current;
    if (!sentinel || !hasMore) return;

    const observer = new IntersectionObserver(
      (observerEntries) => {
        const first = observerEntries[0];
        if (first.isIntersecting) {
          const lastEntry = entriesRef.current[entriesRef.current.length - 1];
          if (lastEntry) {
            loadPage(lastEntry.id);
          }
        }
      },
      { threshold: 0 },
    );

    observer.observe(sentinel);
    return () => observer.disconnect();
  }, [loading, hasMore, loadPage]);

  // Listen for new entries added from the transcription pipeline
  useEffect(() => {
    const unlisten = events.historyUpdatePayload.listen((event) => {
      const payload: HistoryUpdatePayload = event.payload;
      if (payload.action === "added") {
        setEntries((prev) => [
          payload.entry,
          ...prev.filter((entry) => entry.id !== payload.entry.id),
        ]);
      } else if (payload.action === "updated") {
        setEntries((prev) =>
          prev.map((e) => (e.id === payload.entry.id ? payload.entry : e)),
        );
      }
      // "deleted" and "toggled" are handled by optimistic updates only,
      // so we intentionally ignore them here to avoid double-mutation.
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const toggleSaved = async (id: number) => {
    // Optimistic update
    setEntries((prev) =>
      prev.map((e) => (e.id === id ? { ...e, saved: !e.saved } : e)),
    );
    try {
      const result = await commands.toggleHistoryEntrySaved(id);
      if (result.status !== "ok") {
        // Revert on failure
        setEntries((prev) =>
          prev.map((e) => (e.id === id ? { ...e, saved: !e.saved } : e)),
        );
      }
    } catch (error) {
      console.error("Failed to toggle saved status:", error);
      // Revert on failure
      setEntries((prev) =>
        prev.map((e) => (e.id === id ? { ...e, saved: !e.saved } : e)),
      );
    }
  };

  const getAudioUrl = useCallback(
    async (fileName: string) => {
      try {
        const result = await commands.getAudioFilePath(fileName);
        if (result.status === "ok") {
          if (osType === "linux") {
            const fileData = await readFile(result.data);
            const blob = new Blob([fileData], { type: "audio/wav" });
            return URL.createObjectURL(blob);
          }
          return convertFileSrc(result.data, "asset");
        }
        return null;
      } catch (error) {
        console.error("Failed to get audio file path:", error);
        return null;
      }
    },
    [osType],
  );

  const deleteAudioEntry = async (id: number) => {
    // Optimistically remove
    setEntries((prev) => prev.filter((e) => e.id !== id));
    try {
      const result = await commands.deleteHistoryEntry(id);
      if (result.status !== "ok") {
        // Reload on failure
        loadPage();
      }
    } catch (error) {
      console.error("Failed to delete entry:", error);
      loadPage();
    }
  };

  const transcribeFile = async () => {
    try {
      await pickAndTranscribeFiles();
    } catch (error) {
      console.error("Failed to pick a file to transcribe:", error);
      toast.error(t("settings.history.transcribeFailed"));
    }
  };

  const openRecordingsFolder = async () => {
    try {
      const result = await commands.openRecordingsFolder();
      if (result.status !== "ok") {
        throw new Error(String(result.error));
      }
    } catch (error) {
      console.error("Failed to open recordings folder:", error);
    }
  };

  const correctEntryAndSaveWord = useCallback(
    async (
      id: number,
      updatedText: string,
      dictionaryWord: string | null,
    ): Promise<boolean> => {
      try {
        const existingWords =
          useSettingsStore.getState().settings?.custom_words ?? [];
        const alreadyExactMatch =
          dictionaryWord !== null && existingWords.includes(dictionaryWord);
        const result = await commands.correctHistoryEntryAndSaveWord(
          id,
          updatedText,
          dictionaryWord,
        );
        if (result.status !== "ok") {
          toast.error(t("settings.history.correctionSaveError"));
          return false;
        }
        setEntries((prev) => prev.map((e) => (e.id === id ? result.data : e)));
        if (dictionaryWord && !alreadyExactMatch) {
          await useSettingsStore.getState().refreshSettings();
          toast.success(
            t("settings.history.correctionAndWordSaved", {
              word: dictionaryWord,
            }),
          );
        } else {
          toast.success(t("settings.history.correctionSaved"));
        }
        return true;
      } catch (error) {
        console.error("Failed to save corrected history entry:", error);
        toast.error(t("settings.history.correctionSaveError"));
        return false;
      }
    },
    [t],
  );

  let content: React.ReactNode;

  if (loading) {
    content = (
      <div className="px-4 py-3 text-center text-ink-2">
        {t("settings.history.loading")}
      </div>
    );
  } else if (entries.length === 0) {
    content = (
      <div className="px-4 py-3 text-center text-ink-2">
        {t("settings.history.empty")}
      </div>
    );
  } else {
    content = (
      <>
        <AudioPlayerGroup>
          <div className="divide-y divide-line">
            {entries.map((entry) => (
              <HistoryEntryComponent
                key={entry.id}
                entry={entry}
                job={jobs[entry.id]}
                onToggleSaved={() => toggleSaved(entry.id)}
                onCopyText={() =>
                  copyToClipboard(
                    entry.post_processed_text ?? entry.transcription_text,
                  )
                }
                getAudioUrl={getAudioUrl}
                deleteAudio={deleteAudioEntry}
                retryTranscription={commands.retryHistoryEntryTranscription}
                cancelTranscription={commands.cancelHistoryEntryTranscription}
                onCorrectAndSave={correctEntryAndSaveWord}
              />
            ))}
          </div>
        </AudioPlayerGroup>
        {/* Sentinel for infinite scroll */}
        <div ref={sentinelRef} className="h-1" />
      </>
    );
  }

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <div className="space-y-2">
        <div className="px-4 flex items-center justify-between">
          <div>
            <h2 className="text-xs font-medium text-ink-3 uppercase tracking-wide">
              {t("settings.history.title")}
            </h2>
          </div>
          <div className="flex items-center gap-2">
            <HeaderButton
              onClick={transcribeFile}
              label={t("settings.history.transcribeFile")}
              icon={FilePlus}
            />
            <HeaderButton
              onClick={openRecordingsFolder}
              label={t("settings.history.openFolder")}
              icon={FolderOpen}
            />
          </div>
        </div>
        <div className="bg-background border border-line rounded-lg overflow-visible">
          {content}
        </div>
      </div>
    </div>
  );
};

interface HistoryEntryProps {
  entry: HistoryEntry;
  job?: ActiveJobStatus;
  onToggleSaved: () => void;
  onCopyText: () => Promise<boolean>;
  getAudioUrl: (fileName: string) => Promise<string | null>;
  deleteAudio: (id: number) => Promise<void>;
  retryTranscription: (id: number) => Promise<void>;
  cancelTranscription: (id: number) => Promise<void>;
  onCorrectAndSave: (
    id: number,
    updatedText: string,
    dictionaryWord: string | null,
  ) => Promise<boolean>;
}

const HistoryEntryComponent: React.FC<HistoryEntryProps> = ({
  entry,
  job,
  onToggleSaved,
  onCopyText,
  getAudioUrl,
  deleteAudio,
  retryTranscription,
  cancelTranscription,
  onCorrectAndSave,
}) => {
  const { t, i18n } = useTranslation();
  const [showCopied, setShowCopied] = useState(false);
  // Pressed cancel: a running job stops after its current chunk.
  const [cancelling, setCancelling] = useState(false);
  const displayedText = entry.post_processed_text ?? entry.transcription_text;
  const [expanded, setExpanded] = useState(false);
  const [overflows, setOverflows] = useState(false);
  const [isEditing, setIsEditing] = useState(false);
  const [draftText, setDraftText] = useState(displayedText);
  const [dictionaryWord, setDictionaryWord] = useState("");
  const [dictionaryWordTouched, setDictionaryWordTouched] = useState(false);
  const [savingCorrection, setSavingCorrection] = useState(false);
  const textRef = useRef<HTMLParagraphElement>(null);
  const editTextareaRef = useRef<HTMLTextAreaElement>(null);
  const pendingSelectionRef = useRef("");

  // Queued or being transcribed in the background.
  const retrying = job !== undefined;
  useEffect(() => {
    if (!retrying) setCancelling(false);
  }, [retrying]);
  useEffect(() => {
    if (retrying && isEditing) {
      setIsEditing(false);
    }
  }, [retrying, isEditing]);
  useEffect(() => {
    if (!isEditing) {
      setDraftText(displayedText);
    }
  }, [displayedText, isEditing]);

  const hasTranscription = displayedText.trim().length > 0;
  const folded = hasTranscription && !retrying && !expanded;

  const readSelectedEntryText = useCallback((): string => {
    const selection = window.getSelection();
    const container = textRef.current;
    if (!selection || !container || selection.isCollapsed) {
      return "";
    }
    const anchor = selection.anchorNode;
    const focus = selection.focusNode;
    if (
      (anchor && container.contains(anchor)) ||
      (focus && container.contains(focus))
    ) {
      return normalizeDictionaryCandidate(selection.toString());
    }
    return "";
  }, []);

  const handleOpenOrToggleEditor = () => {
    if (!hasTranscription || retrying) return;
    if (isEditing) {
      handleCancelEdit();
      return;
    }
    const selectedWord = pendingSelectionRef.current || readSelectedEntryText();
    pendingSelectionRef.current = "";
    setDraftText(displayedText);
    setDictionaryWord(selectedWord);
    setDictionaryWordTouched(selectedWord.length > 0);
    setIsEditing(true);
    requestAnimationFrame(() => {
      const textarea = editTextareaRef.current;
      if (textarea) {
        textarea.focus();
        const len = textarea.value.length;
        textarea.setSelectionRange(len, len);
      }
    });
  };

  const handleCancelEdit = () => {
    if (savingCorrection) return;
    setIsEditing(false);
    setDraftText(displayedText);
    setDictionaryWord("");
    setDictionaryWordTouched(false);
  };

  const handleDraftChange = (nextValue: string) => {
    setDraftText(nextValue);
    if (!dictionaryWordTouched) {
      setDictionaryWord(suggestDictionaryWord(displayedText, nextValue));
    }
  };

  const normalizedDictionaryWord = normalizeDictionaryCandidate(dictionaryWord);

  const handleSaveCorrection = async () => {
    if (savingCorrection) return;
    const trimmedDraft = draftText.trim();
    if (!trimmedDraft) {
      toast.error(t("settings.history.correctionEmptyError"));
      return;
    }
    setSavingCorrection(true);
    try {
      const ok = await onCorrectAndSave(
        entry.id,
        trimmedDraft,
        normalizedDictionaryWord.length > 0 ? normalizedDictionaryWord : null,
      );
      if (ok) {
        setIsEditing(false);
        setDictionaryWord("");
        setDictionaryWordTouched(false);
      }
    } finally {
      setSavingCorrection(false);
    }
  };

  const handleEditorKeyDown = (e: React.KeyboardEvent) => {
    if (e.nativeEvent.isComposing) {
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      handleCancelEdit();
    } else if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      void handleSaveCorrection();
    }
  };

  // Offer "Show more" only when the folded text is actually cut off.
  useLayoutEffect(() => {
    const el = textRef.current;
    if (!el || !folded) return;
    const measure = () => setOverflows(el.scrollHeight > el.clientHeight + 1);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, [folded, displayedText]);

  const handleLoadAudio = useCallback(
    () => getAudioUrl(entry.file_name),
    [getAudioUrl, entry.file_name],
  );

  const handleCopyText = async () => {
    if (!hasTranscription) {
      return;
    }

    const copied = await onCopyText();
    if (!copied) {
      toast.error(t("settings.history.copyError"));
      return;
    }

    setShowCopied(true);
    setTimeout(() => setShowCopied(false), 2000);
  };

  const handleDeleteEntry = async () => {
    try {
      await deleteAudio(entry.id);
    } catch (error) {
      console.error("Failed to delete entry:", error);
      toast.error(t("settings.history.deleteError"));
    }
  };

  const formattedDate = formatDateTime(String(entry.timestamp), i18n.language);

  let progressText = t("settings.history.transcribing");
  if (job?.state === "queued") {
    progressText = t("settings.history.waiting");
  } else if (job?.state === "running" && job.percent > 0) {
    progressText = t("settings.history.transcribingPercent", {
      percent: job.percent,
    });
  }

  return (
    <div className="px-4 py-2 pb-5 flex flex-col gap-3">
      <div className="flex justify-between items-center gap-3">
        <div className="flex items-center gap-2 min-w-0">
          <p className="text-sm font-medium shrink-0">{formattedDate}</p>
          {entry.source_name && (
            <span
              className="flex items-center gap-1 min-w-0 text-sm text-ink-3"
              title={entry.source_name}
            >
              <FileAudio width={14} height={14} className="shrink-0" />
              <span className="truncate">{entry.source_name}</span>
            </span>
          )}
        </div>
        <div className="flex items-center shrink-0">
          <IconButton
            onMouseDown={() => {
              pendingSelectionRef.current = readSelectedEntryText();
            }}
            onClick={handleOpenOrToggleEditor}
            disabled={!hasTranscription || retrying}
            active={isEditing}
            title={t("settings.history.correctAndSave")}
          >
            <BookPlus width={16} height={16} />
          </IconButton>
          <IconButton
            onClick={handleCopyText}
            disabled={!hasTranscription || retrying}
            title={t("settings.history.copyToClipboard")}
          >
            {showCopied ? (
              <Check width={16} height={16} />
            ) : (
              <Copy width={16} height={16} />
            )}
          </IconButton>
          <IconButton
            onClick={onToggleSaved}
            active={entry.saved}
            title={
              entry.saved
                ? t("settings.history.unsave")
                : t("settings.history.save")
            }
          >
            <Star
              width={16}
              height={16}
              fill={entry.saved ? "currentColor" : "none"}
            />
          </IconButton>
          {retrying ? (
            <IconButton
              onClick={() => {
                setCancelling(true);
                void cancelTranscription(entry.id);
              }}
              disabled={cancelling}
              title={t("settings.history.cancelTranscription")}
            >
              <X width={16} height={16} />
            </IconButton>
          ) : (
            <IconButton
              onClick={() => retryTranscription(entry.id)}
              title={t("settings.history.retranscribe")}
            >
              <RotateCcw width={16} height={16} />
            </IconButton>
          )}
          <IconButton
            onClick={handleDeleteEntry}
            title={t("settings.history.delete")}
          >
            <Trash2 width={16} height={16} />
          </IconButton>
        </div>
      </div>

      <div className="flex flex-col items-start gap-1 pb-2">
        <p
          ref={textRef}
          className={`italic text-sm w-full ${
            retrying
              ? ""
              : hasTranscription
                ? "text-text select-text cursor-text whitespace-pre-wrap break-words"
                : "text-ink-3"
          }`}
          style={
            retrying
              ? { animation: "transcribe-pulse 3s ease-in-out infinite" }
              : folded
                ? {
                    display: "-webkit-box",
                    WebkitLineClamp: FOLDED_LINES,
                    WebkitBoxOrient: "vertical",
                    overflow: "hidden",
                  }
                : undefined
          }
        >
          {retrying && (
            <style>{`
            @keyframes transcribe-pulse {
              0%, 100% { color: var(--color-ink-3); }
              50% { color: var(--color-text); }
            }
          `}</style>
          )}
          {retrying
            ? progressText
            : hasTranscription
              ? displayedText
              : entry.source_name
                ? // Cancelled, interrupted by a quit, or failed.
                  t("settings.history.notTranscribed")
                : t("settings.history.transcriptionFailed")}
        </p>
        {hasTranscription && !retrying && (expanded || overflows) && (
          <button
            type="button"
            onClick={() => setExpanded(!expanded)}
            className="text-xs font-medium text-ink-3 hover:text-logo-primary cursor-pointer"
          >
            {expanded
              ? t("settings.history.showLess")
              : t("settings.history.showMore")}
          </button>
        )}
        {isEditing && !retrying && (
          <div
            data-testid={`history-inline-editor-${entry.id}`}
            onKeyDown={handleEditorKeyDown}
            className="w-full mt-2 p-3 rounded-md border border-line bg-glass-bg/60 flex flex-col gap-2.5"
          >
            <div className="flex flex-col gap-1">
              <label
                htmlFor={`history-transcript-${entry.id}`}
                className="text-xs font-medium text-ink-3"
              >
                {t("settings.history.correctedTranscriptLabel")}
              </label>
              <textarea
                id={`history-transcript-${entry.id}`}
                ref={editTextareaRef}
                rows={2}
                value={draftText}
                disabled={savingCorrection}
                placeholder={t(
                  "settings.history.correctedTranscriptPlaceholder",
                )}
                onChange={(e) => handleDraftChange(e.target.value)}
                className="w-full px-2.5 py-1.5 text-sm rounded-[5px] border border-mid-gray/60 bg-glass-bg text-text focus:outline-none focus:border-logo-primary transition-colors resize-y"
              />
            </div>
            <div className="flex flex-col sm:flex-row sm:items-center gap-2">
              <label
                htmlFor={`history-dict-word-${entry.id}`}
                className="text-xs font-medium text-ink-3 shrink-0"
              >
                {t("settings.history.dictionaryWordLabel")}
              </label>
              <Input
                id={`history-dict-word-${entry.id}`}
                type="text"
                variant="compact"
                maxLength={50}
                value={dictionaryWord}
                disabled={savingCorrection}
                placeholder={t("settings.history.dictionaryWordPlaceholder")}
                onChange={(e) => {
                  setDictionaryWordTouched(true);
                  setDictionaryWord(e.target.value);
                }}
                className="flex-1"
              />
            </div>
            <div className="flex items-center justify-end gap-2">
              <Button
                type="button"
                variant="secondary"
                size="sm"
                disabled={savingCorrection}
                onClick={handleCancelEdit}
              >
                {t("settings.history.cancelEdit")}
              </Button>
              <Button
                type="button"
                variant="primary"
                size="sm"
                disabled={savingCorrection || draftText.trim().length === 0}
                onClick={() => void handleSaveCorrection()}
              >
                {savingCorrection
                  ? t("settings.history.savingCorrection")
                  : normalizedDictionaryWord.length > 0
                    ? t("settings.history.saveAndAddWord")
                    : t("settings.history.saveCorrection")}
              </Button>
            </div>
          </div>
        )}
      </div>

      <AudioPlayer onLoadRequest={handleLoadAudio} className="w-full" />
    </div>
  );
};
