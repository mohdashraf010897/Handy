import React, { useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Check,
  Copy,
  RefreshCw,
  Sparkles,
  Trash2,
} from "lucide-react";
import { Button } from "../ui/Button";
import { Input } from "../ui/Input";
import { Textarea } from "../ui/Textarea";
import {
  MEETING_TEMPLATES,
  type MeetingDetail,
  type MeetingTemplate,
  formatDurationMmSs,
  formatSegmentSpeakerBadge,
  isolateBidi,
} from "./api";

type DetailTab = "notes" | "bullets" | "transcript";

interface MeetingDetailPaneProps {
  detail: MeetingDetail | null;
  isBusy: boolean;
  onUpdateMeta: (params: {
    meetingId: string;
    title?: string | null;
    roughNotes?: string | null;
    template?: MeetingTemplate | null;
  }) => Promise<void>;
  onRegenerate: (
    meetingId: string,
    template?: MeetingTemplate | null,
  ) => Promise<void>;
  onSaveAndReexpand: (
    meetingId: string,
    roughNotes: string,
    template: MeetingTemplate,
  ) => Promise<void>;
  onCopyMarkdown: (meetingId: string) => Promise<void>;
  onDelete: (meetingId: string) => Promise<void>;
}

/**
 * Renders inline `**bold**` spans as React elements (never `dangerouslySetInnerHTML`).
 */
function renderInlineBold(text: string): React.ReactNode[] {
  const parts = text.split(/(\*\*[^*]+\*\*)/g);
  return parts.map((part, idx) => {
    if (part.startsWith("**") && part.endsWith("**") && part.length > 4) {
      return (
        <strong key={idx} className="font-semibold text-text">
          {isolateBidi(part.slice(2, -2))}
        </strong>
      );
    }
    return <React.Fragment key={idx}>{isolateBidi(part)}</React.Fragment>;
  });
}

/**
 * Safe, zero-XSS React Markdown renderer for headings (`#`/`##`/`###`),
 * bullet lists (`- ` / `* `), and paragraphs with inline `**bold**`.
 */
const SafeMarkdownView: React.FC<{ markdown: string; emptyLabel: string }> = ({
  markdown,
  emptyLabel,
}) => {
  const trimmed = markdown.trim();
  if (!trimmed) {
    return <p className="text-sm text-ink-3 italic">{emptyLabel}</p>;
  }

  const lines = trimmed.split(/\r?\n/);
  const blocks: React.ReactNode[] = [];
  let currentBullets: string[] = [];

  const flushBullets = (keyPrefix: number) => {
    if (currentBullets.length === 0) return;
    const items = currentBullets;
    currentBullets = [];
    blocks.push(
      <ul
        key={`ul-${keyPrefix}`}
        className="list-disc ps-5 space-y-1.5 text-sm text-text leading-relaxed"
      >
        {items.map((item, idx) => (
          <li key={idx} dir="auto">
            {renderInlineBold(item)}
          </li>
        ))}
      </ul>,
    );
  };

  lines.forEach((rawLine, index) => {
    const line = rawLine.trim();
    if (!line) {
      flushBullets(index);
      return;
    }

    const bulletMatch = /^[-*•]\s+(.*)$/.exec(line);
    if (bulletMatch) {
      currentBullets.push(bulletMatch[1]);
      return;
    }

    flushBullets(index);

    if (line.startsWith("### ")) {
      blocks.push(
        <h4
          key={`h3-${index}`}
          dir="auto"
          className="text-xs font-semibold uppercase tracking-wide text-ink-2 pt-2"
        >
          {renderInlineBold(line.slice(4))}
        </h4>,
      );
      return;
    }

    if (line.startsWith("## ") || line.startsWith("# ")) {
      const headingText = line.replace(/^#{1,2}\s+/, "");
      blocks.push(
        <h3
          key={`h2-${index}`}
          dir="auto"
          className="text-sm font-semibold text-text pt-2 border-b border-line pb-1"
        >
          {renderInlineBold(headingText)}
        </h3>,
      );
      return;
    }

    blocks.push(
      <p
        key={`p-${index}`}
        dir="auto"
        className="text-sm text-ink-2 leading-relaxed"
      >
        {renderInlineBold(line)}
      </p>,
    );
  });

  flushBullets(lines.length);

  return <div className="space-y-3">{blocks}</div>;
};

export const MeetingDetailPane: React.FC<MeetingDetailPaneProps> = ({
  detail,
  isBusy,
  onUpdateMeta,
  onRegenerate,
  onSaveAndReexpand,
  onCopyMarkdown,
  onDelete,
}) => {
  const { t } = useTranslation();
  const titleId = useId();
  const templateId = useId();
  const bulletsId = useId();

  const [activeTab, setActiveTab] = useState<DetailTab>("notes");
  const [draftTitle, setDraftTitle] = useState("");
  const [draftTemplate, setDraftTemplate] =
    useState<MeetingTemplate>("general");
  const [draftBullets, setDraftBullets] = useState("");
  const [copied, setCopied] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const copyTimerRef = useRef<ReturnType<typeof setTimeout>>();

  useEffect(() => {
    if (!detail) return;
    setDraftTitle(detail.summary.title);
    setDraftTemplate(detail.summary.template);
    setDraftBullets(detail.rough_notes);
    setConfirmDelete(false);
  }, [
    detail?.summary.id,
    detail?.summary.title,
    detail?.summary.template,
    detail?.rough_notes,
  ]);

  useEffect(() => {
    return () => {
      clearTimeout(copyTimerRef.current);
    };
  }, []);

  if (!detail) {
    return (
      <div className="bg-background border border-line rounded-lg p-6 flex items-center justify-center min-h-[260px]">
        <p className="text-sm text-ink-3 text-center">
          {t("meetings.detail.selectPrompt")}
        </p>
      </div>
    );
  }

  const { summary } = detail;

  const commitTitle = async () => {
    const trimmed = draftTitle.trim();
    if (!trimmed || trimmed === summary.title) {
      setDraftTitle(summary.title);
      return;
    }
    await onUpdateMeta({
      meetingId: summary.id,
      title: trimmed,
      template: draftTemplate,
      roughNotes: draftBullets,
    });
  };

  const handleTemplateChange = async (nextTemplate: MeetingTemplate) => {
    setDraftTemplate(nextTemplate);
    await onUpdateMeta({
      meetingId: summary.id,
      title: draftTitle.trim() || summary.title,
      template: nextTemplate,
      roughNotes: draftBullets,
    });
  };

  const handleCopy = async () => {
    await onCopyMarkdown(summary.id);
    setCopied(true);
    clearTimeout(copyTimerRef.current);
    copyTimerRef.current = setTimeout(() => setCopied(false), 2000);
  };

  const summarySourceLabel = (() => {
    switch (detail.summary_source) {
      case "llm":
        return t("meetings.detail.source.llm");
      case "apple_intelligence":
        return t("meetings.detail.source.apple_intelligence");
      default:
        return t("meetings.detail.source.local_extractive");
    }
  })();

  const formattedDuration = formatDurationMmSs(summary.duration_ms);
  const tabs: readonly DetailTab[] = ["notes", "bullets", "transcript"];

  return (
    <div className="bg-background border border-line rounded-lg overflow-hidden divide-y divide-line">
      {/* Header: Editable Title + Template Picker + Actions */}
      <div className="p-4 space-y-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="min-w-[13rem] flex-1">
            <Input
              id={titleId}
              variant="default"
              dir="auto"
              value={draftTitle}
              disabled={isBusy}
              aria-label={t("meetings.detail.titleAriaLabel")}
              onChange={(e) => setDraftTitle(e.target.value)}
              onBlur={() => void commitTitle()}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  void commitTitle();
                } else if (e.key === "Escape") {
                  setDraftTitle(summary.title);
                }
              }}
              className="w-full"
            />
          </div>

          <select
            id={templateId}
            value={draftTemplate}
            disabled={isBusy}
            aria-label={t("meetings.detail.templateAriaLabel")}
            onChange={(e) =>
              void handleTemplateChange(e.target.value as MeetingTemplate)
            }
            className="px-2.5 py-1.5 text-xs font-semibold bg-wash border border-line-strong rounded-md text-text hover:border-logo-primary focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary cursor-pointer"
          >
            {MEETING_TEMPLATES.map((tpl) => (
              <option key={tpl} value={tpl}>
                {t(`meetings.templates.${tpl}`)}
              </option>
            ))}
          </select>
        </div>

        {/* Metadata Badges + Action Buttons */}
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex flex-wrap items-center gap-2">
            {/* Summary Source Transparency Badge */}
            <span className="inline-flex items-center gap-1 px-2.5 py-0.5 rounded-full text-xs font-medium bg-logo-primary/15 border border-logo-primary/30 text-text">
              <Sparkles className="w-3 h-3" aria-hidden="true" />
              <span>{summarySourceLabel}</span>
            </span>

            <span className="font-mono text-xs text-ink-3 tabular-nums">
              {formattedDuration}
            </span>

            {summary.app_name && (
              <span className="text-xs text-ink-3">
                {isolateBidi(summary.app_name)}
              </span>
            )}
          </div>

          <div className="flex flex-wrap items-center gap-1.5">
            <Button
              variant="secondary"
              size="sm"
              disabled={isBusy}
              aria-label={t("meetings.detail.regenerate")}
              onClick={() => void onRegenerate(summary.id, draftTemplate)}
              className="flex items-center gap-1.5"
            >
              <RefreshCw
                className={`w-3.5 h-3.5 ${isBusy ? "animate-spin motion-reduce:animate-none" : ""}`}
                aria-hidden="true"
              />
              <span>
                {isBusy
                  ? t("meetings.detail.regenerating")
                  : t("meetings.detail.regenerate")}
              </span>
            </Button>

            <Button
              variant="secondary"
              size="sm"
              disabled={isBusy}
              aria-label={t("meetings.detail.copyMarkdown")}
              onClick={() => void handleCopy()}
              className="flex items-center gap-1.5"
            >
              {copied ? (
                <>
                  <Check className="w-3.5 h-3.5 text-moss" aria-hidden="true" />
                  <span>{t("meetings.detail.copied")}</span>
                </>
              ) : (
                <>
                  <Copy className="w-3.5 h-3.5" aria-hidden="true" />
                  <span>{t("meetings.detail.copyMarkdown")}</span>
                </>
              )}
            </Button>

            {confirmDelete ? (
              <div className="inline-flex items-center gap-1">
                <Button
                  variant="danger"
                  size="sm"
                  disabled={isBusy}
                  aria-label={t("meetings.detail.confirmDelete")}
                  onClick={() => void onDelete(summary.id)}
                >
                  {t("meetings.detail.confirmDelete")}
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={isBusy}
                  aria-label={t("meetings.detail.cancelDelete")}
                  onClick={() => setConfirmDelete(false)}
                >
                  {t("meetings.detail.cancelDelete")}
                </Button>
              </div>
            ) : (
              <Button
                variant="danger-ghost"
                size="sm"
                disabled={isBusy}
                aria-label={t("meetings.detail.delete")}
                title={t("meetings.detail.delete")}
                onClick={() => setConfirmDelete(true)}
                className="flex items-center gap-1"
              >
                <Trash2 className="w-3.5 h-3.5" aria-hidden="true" />
                <span>{t("meetings.detail.delete")}</span>
              </Button>
            )}
          </div>
        </div>
      </div>

      {/* 3-Tab Switcher: Notes | Your Bullets | Transcript */}
      <div
        role="tablist"
        aria-label={t("meetings.title")}
        className="flex items-center gap-1 px-4 py-2 bg-wash/50"
      >
        {tabs.map((tab) => {
          const isSelected = activeTab === tab;
          return (
            <button
              key={tab}
              type="button"
              role="tab"
              aria-selected={isSelected}
              onClick={() => setActiveTab(tab)}
              className={`px-3 py-1.5 rounded-md text-xs font-semibold transition-colors cursor-pointer focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary ${
                isSelected
                  ? "bg-background-ui text-white"
                  : "text-ink-2 hover:bg-logo-primary/15 hover:text-text"
              }`}
            >
              {t(`meetings.detail.tabs.${tab}`)}
            </button>
          );
        })}
      </div>

      {/* Tab Content */}
      <div className="p-4">
        {activeTab === "notes" && (
          <div className="space-y-4">
            <SafeMarkdownView
              markdown={detail.expanded_notes_md}
              emptyLabel={t("meetings.detail.notesEmpty")}
            />

            {detail.rollups.length > 0 && (
              <details className="group border border-line rounded-md bg-wash/40">
                <summary className="flex cursor-pointer list-none items-center justify-between px-3 py-2 text-xs font-semibold text-ink-2 hover:bg-wash focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary [&::-webkit-details-marker]:hidden">
                  <span>
                    {`${t("meetings.detail.rollupsTitle")} (${detail.rollups.length})`}
                  </span>
                  <span
                    aria-hidden="true"
                    className="text-ink-3 transition-transform group-open:rotate-90"
                  >
                    {"›"}
                  </span>
                </summary>
                <div className="divide-y divide-line border-t border-line p-3 space-y-2">
                  {detail.rollups.map((rollup) => {
                    const rangeLabel = `[${formatDurationMmSs(rollup.start_ms)} – ${formatDurationMmSs(rollup.end_ms)}]`;
                    return (
                      <div key={rollup.id} className="pt-2 first:pt-0 space-y-1">
                        <span className="font-mono text-xs text-ink-3 tabular-nums">
                          {rangeLabel}
                        </span>
                        <SafeMarkdownView
                          markdown={rollup.summary_bullets}
                          emptyLabel={t("meetings.detail.notesEmpty")}
                        />
                      </div>
                    );
                  })}
                </div>
              </details>
            )}
          </div>
        )}

        {activeTab === "bullets" && (
          <div className="space-y-3">
            <label htmlFor={bulletsId} className="sr-only">
              {t("meetings.detail.tabs.bullets")}
            </label>
            <Textarea
              id={bulletsId}
              dir="auto"
              value={draftBullets}
              disabled={isBusy}
              placeholder={t("meetings.detail.bulletsPlaceholder")}
              aria-label={t("meetings.detail.tabs.bullets")}
              onChange={(e) => setDraftBullets(e.target.value)}
              className="w-full min-h-[180px] font-normal text-sm leading-relaxed"
            />
            <div className="flex justify-end">
              <Button
                variant="primary"
                size="sm"
                disabled={isBusy}
                aria-label={t("meetings.detail.saveAndReexpand")}
                onClick={() =>
                  void onSaveAndReexpand(
                    summary.id,
                    draftBullets,
                    draftTemplate,
                  ).then(() => setActiveTab("notes"))
                }
                className="flex items-center gap-1.5"
              >
                <Sparkles className="w-3.5 h-3.5" aria-hidden="true" />
                <span>{t("meetings.detail.saveAndReexpand")}</span>
              </Button>
            </div>
          </div>
        )}

        {activeTab === "transcript" && (
          <div className="max-h-[360px] overflow-y-auto space-y-2.5 pe-1">
            {detail.segments.length === 0 ? (
              <p className="text-sm text-ink-3 italic">
                {t("meetings.detail.transcriptEmpty")}
              </p>
            ) : (
              detail.segments.map((seg) => {
                const isMe = seg.speaker === "me";
                const timeTag = `[${formatDurationMmSs(seg.start_ms)}]`;
                return (
                  <div
                    key={seg.id}
                    className="flex items-start gap-2.5 text-sm leading-relaxed"
                  >
                    <span className="font-mono text-xs text-ink-3 shrink-0 pt-0.5 tabular-nums">
                      {timeTag}
                    </span>
                    <span
                      title={formatSegmentSpeakerBadge(
                        seg,
                        t("meetings.speaker.me"),
                        t("meetings.speaker.others"),
                      )}
                      className={`px-2 py-0.5 rounded text-xs font-semibold shrink-0 max-w-44 truncate ${
                        isMe
                          ? "bg-logo-primary/20 text-text"
                          : "bg-line text-ink-2"
                      }`}
                    >
                      {isolateBidi(
                        formatSegmentSpeakerBadge(
                          seg,
                          t("meetings.speaker.me"),
                          t("meetings.speaker.others"),
                        ),
                      )}
                    </span>
                    <p dir="auto" className="text-text flex-1 break-words">
                      {isolateBidi(seg.text)}
                    </p>
                  </div>
                );
              })
            )}
          </div>
        )}
      </div>
    </div>
  );
};
