import React, { Fragment, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ChevronDown, Cpu, Globe, Zap } from "lucide-react";
import { useOsType } from "@/hooks/useOsType";
import type { OSType } from "@/lib/utils/keyboard";
import { Button } from "../../ui/Button";
import { Radio } from "../../ui/Radio";
import { GradeChip } from "../../shared/GradeChip";
import { Tooltip } from "../../ui/Tooltip";
import { ProgressLine } from "../../shared/ProgressLine";
import { SpeedBars, speedLevel } from "../../shared/SpeedBars";
import { Tag } from "../../shared/Tag";
import {
  Codes,
  GradeTip,
  SpeedTip,
  Values,
  useSpeedWords,
} from "../../shared/modelTable/columns";
import type { TableModel } from "../../shared/modelTable/context";
import { toTableModel } from "../../shared/modelTable/tableModel";
import {
  CAPABILITIES,
  CAPABILITY_KEYS,
  type Capability,
} from "../../shared/modelTable/capabilities";
import {
  ModelTable,
  NO_FILTERS,
  useModelFilters,
  type TableSection,
} from "../../shared/modelTable/ModelTable";
import {
  ModelTableRow,
  ROLE_KEYS as PICK_ROLE_KEYS,
} from "../../shared/modelTable/ModelTableRow";
import { formatModelSize } from "@/lib/utils/format";
import { OnboardingShell, ScreenHeader, ScreenMain } from "../OnboardingShell";
import { useOnboardingStore } from "../onboardingStore";
import { useDownloadWords } from "../DownloadStatus";
import { languageName } from "@/lib/constants/languages";
import type { CatalogModel, ModelRow, PickRole } from "@/lib/models/catalog";
import type { useOnboardingModels } from "../useOnboardingModels";
import { moveModels } from "./modelTransitions";
import "./modelTransitions.css";

type Models = ReturnType<typeof useOnboardingModels>;

/** A card's words for each pick: the Full card's line and the short ones. */
const ROLE_KEYS: Record<PickRole | "extra", string> = {
  ...PICK_ROLE_KEYS,
  extra: "fromAll",
};

/** A printable key with no shortcut modifier, Space aside. */
const isTyping = (e: KeyboardEvent) =>
  e.key.length === 1 && e.key !== " " && !e.metaKey && !e.ctrlKey && !e.altKey;

const Chevron = () => (
  <span
    aria-hidden="true"
    className="inline-block size-[6px] shrink-0 border-e-[1.5px] border-b-[1.5px] border-current -mt-[3px] rotate-45"
  />
);

/** The view-transition name of a model's part, the same in every state. */
const vt = (id: string, part = "") => ({
  "data-vt":
    "mf-" + id.toLowerCase().replace(/[^a-z0-9]+/g, "-") + (part && "-" + part),
});

import {
  MACOS_NATIVE_SPEECH_ID,
  resolveMacZeroWaitTarget,
} from "../macZeroWaitHelpers";

export { MACOS_NATIVE_SPEECH_ID, resolveMacZeroWaitTarget };

interface ModelProps {
  progress?: { index: number; count: number };
  os?: OSType;
  models: Models;
  onBack?: () => void;
  /** The picked model (catalog id or "macos-native-speech"), once Download or Continue is pressed. */
  onChoose: (modelId: string) => void;
}

/** Screen 3: pick one of three suggested models, or Show all for the table. */
export const Model: React.FC<ModelProps> = ({
  progress,
  os,
  models,
  onBack,
  onChoose,
}) => {
  const { t, i18n } = useTranslation();
  const detectedOs = useOsType();
  const isMac = (os ?? detectedOs) === "macos";
  const words = useDownloadWords();
  const speedWords = useSpeedWords();
  const { modelId, extraModelId, downloadId, set } = useOnboardingStore();
  const [showAll, setShowAll] = useState(false);
  const { filters, setFilters, matches } = useModelFilters();
  const rootRef = useRef<HTMLDivElement>(null);

  const { picks, groups, offered, grading } = models;
  const langs = grading.langs;
  const dense = langs.length > 5;
  // Shown regardless of settings: they're capabilities, as on the Models page.
  const has = (m: CatalogModel, c: Capability) =>
    m.capabilities[CAPABILITIES[c].flag];
  const previews = (m: CatalogModel) => has(m, "preview");
  const pick = picks.find((p) => p.role === "pick") ?? picks[0];

  const byId = useMemo(() => new Map(offered.map((m) => [m.id, m])), [offered]);
  const {
    englishGoalModel,
    multilingualGoalModel,
    isEnglishOnly,
    autoHdModel,
  } = useMemo(
    () =>
      resolveMacZeroWaitTarget({
        offered,
        pickModel: pick?.model,
        langs,
      }),
    [offered, pick?.model, langs],
  );

  const selectedId =
    modelId ??
    downloadId ??
    (isMac ? autoHdModel?.id : undefined) ??
    pick?.model.id ??
    (isMac ? MACOS_NATIVE_SPEECH_ID : null);

  const isNativeSelected = selectedId === MACOS_NATIVE_SPEECH_ID;
  const isPick = (id: string | null) =>
    !!id &&
    (id === MACOS_NATIVE_SPEECH_ID || picks.some((p) => p.model.id === id));
  // A model picked from the full list stays as a fourth card unless a pick.
  const extraId =
    selectedId && !isPick(selectedId)
      ? selectedId
      : isPick(extraModelId)
        ? null
        : extraModelId;
  useEffect(() => {
    if (extraId !== extraModelId) set({ extraModelId: extraId });
  }, [extraId, extraModelId, set]);

  const isNonGoalSelected =
    !!selectedId &&
    selectedId !== MACOS_NATIVE_SPEECH_ID &&
    !!(englishGoalModel || multilingualGoalModel) &&
    selectedId !== englishGoalModel?.id &&
    selectedId !== multilingualGoalModel?.id;
  const [advancedOpen, setAdvancedOpen] = useState(() => isNonGoalSelected);
  useEffect(() => {
    if (isNonGoalSelected) setAdvancedOpen(true);
  }, [isNonGoalSelected]);

  const rowFor = (id: string): ModelRow | undefined =>
    [...groups.picks, ...groups.others, ...groups.partial].find(
      (r) => r.model.id === id,
    );

  const ui = i18n.language;
  const nameOf = (code: string) => languageName(code, ui);
  const sizeOf = (m: CatalogModel) =>
    formatModelSize(models.infoFor(m)?.size_mb);
  /** The offered models as the table shows them, by catalog id like the picks. */
  const entriesFor = (rows: ModelRow[]): TableModel[] =>
    rows.flatMap((row) => {
      const info = models.infoFor(row.model);
      return info ? [{ ...toTableModel(info, t), id: row.model.id }] : [];
    });

  const capabilityName = (c: Capability) => t(`modelTable.tags.${c}`);
  const capabilityLine = (c: Capability) => t(`modelTable.tags.${c}Title`);

  /** A capability's tag on a card: its icon and word. */
  const capabilityTag = (m: CatalogModel, c: Capability) => {
    const { Icon } = CAPABILITIES[c];
    return (
      has(m, c) && (
        <Tooltip text={capabilityLine(c)}>
          <Tag className="inline-flex items-center gap-[4px]">
            <Icon
              aria-hidden="true"
              className="size-[11px]"
              strokeWidth={2.25}
            />
            {capabilityName(c)}
          </Tag>
        </Tooltip>
      )
    );
  };

  /** What a model can do besides transcribe, beside its role. */
  const capabilityTags = (m: CatalogModel) =>
    CAPABILITY_KEYS.map((c) => (
      <Fragment key={c}>{capabilityTag(m, c)}</Fragment>
    ));

  const move = (update: () => void) => moveModels(rootRef.current, update);

  const open = (initialQuery = "") =>
    move(() => {
      setFilters({ ...NO_FILTERS, query: initialQuery });
      setShowAll(true);
    });
  const close = () =>
    move(() => {
      setShowAll(false);
      setFilters(NO_FILTERS);
    });

  // The extra model follows from modelId (extraId above).
  const choose = (id: string, animate: boolean) => {
    const update = () => set({ modelId: id });
    if (animate) move(update);
    else update();
  };

  // Typing on Model opens every model with what's typed in the search.
  useEffect(() => {
    if (showAll) return;
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (target && /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName)) return;
      if (!isTyping(e)) return;
      e.preventDefault();
      open(e.key);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
    // open() is stable enough for a listener re-added with showAll
  }, [showAll]);

  if ((!pick && !isMac) || !selectedId) return null;
  const selected = isNativeSelected
    ? undefined
    : (byId.get(selectedId) ?? pick?.model);

  /* ---------------- the download ---------------- */

  // The downloading model's card shows progress; a failed one, its full card.
  const loading = downloadId ? byId.get(downloadId) : undefined;
  const loadingState = models.downloadState(loading);
  const lineFor = (id: string) =>
    id === downloadId &&
    (loadingState.state === "downloading" || loadingState.state === "checking");
  /** The full card says so, stopped included. */
  const showsDownload = (id: string) =>
    lineFor(id) || (id === downloadId && loadingState.state === "failed");
  const lineValue =
    loadingState.state === "downloading" ? loadingState.progress : 1;
  /** A small card's progress line. */
  const thinLine = (place: string) => (
    <ProgressLine
      value={lineValue}
      busy={loadingState.state === "checking"}
      className={`absolute h-[3px] ${place}`}
    />
  );

  /** The full card's line, with how far on the left and the speed on the right. */
  const fullLine = () => {
    const s = loadingState;
    if (s.state === "failed")
      return (
        <span className="mt-[10px] text-label text-warning">
          {t("onboarding.download.failed")}
        </span>
      );
    if (s.state !== "downloading" && s.state !== "checking") return null;
    let left: string;
    let right: string | null = null;
    if (s.state === "downloading") {
      const { done, total } = words.sizes(
        s,
        loading && models.infoFor(loading)?.size_mb,
      );
      const speed = words.speed(s.bytesPerSec);
      const time = words.time(s.etaSeconds);
      left = t("onboarding.download.cardProgress", {
        percent: words.percent(s),
        done,
        total,
      });
      right =
        speed && time
          ? t("onboarding.download.speedLeft", { speed, time })
          : (speed ?? (time && t("onboarding.download.timeLeft", { time })));
    } else left = t("onboarding.download.checking");
    return (
      <span className="mt-[10px] flex flex-col gap-[5px] self-stretch">
        <ProgressLine value={lineValue} busy={s.state === "checking"} />
        <span className="flex justify-between gap-[12px] text-label tabular-nums text-ink-3">
          <span className="truncate">{left}</span>
          {right && <span className="shrink-0">{right}</span>}
        </span>
      </span>
    );
  };

  /* ---------------- the pieces ---------------- */

  const mark = (id: string, on: boolean) => (
    <span {...vt(id, "m")}>
      <Radio checked={on} />
    </span>
  );

  const roleBase = "shrink-0 font-[650] uppercase tracking-[0.04em]";
  const sizeClass =
    "ms-auto min-w-[50px] text-end text-label text-ink-3 tabular-nums";

  /** The selected model's card. */
  const fullCard = (row: ModelRow, role: PickRole | "extra") => {
    const id = row.model.id;
    return (
      <div
        key={id}
        {...vt(id)}
        data-vt-row=""
        role="radio"
        aria-checked="true"
        tabIndex={0}
        className="flex w-full min-w-0 flex-col items-stretch rounded-panel border-[1.5px] border-plum-pick bg-background px-5 py-4 text-start text-small text-text [view-transition-class:mf-row_mf-box]"
      >
        <span className="flex min-w-0 items-center gap-[10px] whitespace-nowrap">
          {mark(id, true)}
          <b {...vt(id, "r")} className={`${roleBase} text-[16px] text-text`}>
            {t(`modelTable.roles.${ROLE_KEYS[role]}`)}
          </b>
          {capabilityTags(row.model)}
          <span {...vt(id, "s")} className={sizeClass}>
            {sizeOf(row.model)}
          </span>
        </span>
        <span className="flex flex-col items-start">
          <span
            {...vt(id, "n")}
            className="mt-[4px] font-display text-name font-[650] leading-[1.2] tracking-[-0.01em] text-ink-2"
          >
            {row.model.name}
          </span>
          {/* With two rows of grades, the download takes this line's room. */}
          {!(dense && showsDownload(id)) && (
            <span {...vt(id, "d")} className="mt-[2px] text-ink-2">
              {role === "extra"
                ? row.model.description
                : t(`onboarding.model.lines.${ROLE_KEYS[role]}`)}
            </span>
          )}
          <span className="mt-[10px] grid grid-cols-[84px_1fr] items-center gap-x-[12px] gap-y-[8px] self-stretch">
            <span className="text-ink-2">{t("modelTable.accuracy")}</span>
            <span
              className={`grid gap-x-[18px] gap-y-[5px] ${dense ? "grid-cols-[repeat(4,max-content)]" : "grid-cols-[repeat(3,max-content)]"}`}
            >
              {langs.map((l) => {
                const grade = grading.grade(row.model, l);
                return (
                  <GradeTip
                    key={l}
                    grading={grading}
                    model={row.model}
                    langs={[l]}
                  >
                    <span className="inline-flex items-center gap-[6px]">
                      <GradeChip letter={grade.letter} label={grade.label} />
                      <small className="text-small">
                        {dense ? l.toUpperCase() : nameOf(l)}
                      </small>
                    </span>
                  </GradeTip>
                );
              })}
            </span>
            <span className="text-ink-2">{t("modelTable.speed")}</span>
            {/* Over the bars and words, not the whole column. */}
            <SpeedTip
              grading={grading}
              model={row.model}
              streaming={previews(row.model)}
              className="justify-self-start"
            >
              <span className="flex flex-wrap items-center gap-x-[12px] gap-y-[5px]">
                <SpeedBars level={speedLevel(row.wait)} />
                {/* Plain: the Live preview tag on line 1 says it previews. */}
                {speedWords(row.wait)}
              </span>
            </SpeedTip>
          </span>
        </span>
        {showsDownload(id) && fullLine()}
      </div>
    );
  };

  /** The table's codes (EN … SPEED SIZE) over the table's values. */
  const columns = (row: ModelRow) => {
    const id = row.model.id;
    return (
      <span className="flex flex-col gap-[5px]">
        <Codes langs={langs} vt={(part) => vt(id, part)} />
        <Values
          grading={grading}
          model={row.model}
          streaming={previews(row.model)}
          sizeMb={Number(models.infoFor(row.model)?.size_mb ?? 0)}
          vt={(part) => vt(id, part)}
        />
      </span>
    );
  };

  /** A pick that isn't selected: role, name and few words, then columns. */
  const card = (row: ModelRow, role: PickRole) => {
    const id = row.model.id;
    const dim = row.missing.length > 0;
    const line = lineFor(id);
    return (
      <button
        key={id}
        type="button"
        role="radio"
        aria-checked="false"
        {...vt(id)}
        data-vt-row=""
        onClick={() => choose(id, true)}
        className={`relative grid w-full min-w-0 cursor-pointer grid-cols-[18px_minmax(0,1fr)_auto] items-center gap-x-[10px] rounded-card border-[1.5px] border-line bg-background px-5 text-start text-small leading-[1.5] text-text hover:border-ink-3 hover:bg-wash [view-transition-class:mf-row_mf-box] ${line ? "pt-[8px] pb-[15px]" : "py-[8px]"}`}
      >
        {mark(id, false)}
        <span className="flex min-w-0 flex-col items-start">
          <span className="flex min-w-0 max-w-full items-baseline gap-[8px] whitespace-nowrap">
            <b {...vt(id, "r")} className={`${roleBase} text-label text-text`}>
              {t(`modelTable.roles.${ROLE_KEYS[role]}`)}
            </b>
            {capabilityTags(row.model)}
          </span>
          <span className="flex min-w-0 max-w-full items-baseline gap-[8px] whitespace-nowrap">
            <span
              {...vt(id, "n")}
              className={`min-w-0 shrink overflow-hidden text-ellipsis text-small font-semibold text-ink-2 ${dim ? "opacity-60" : ""}`}
            >
              {row.model.name}
            </span>
            {langs.length <= 3 && (
              <span
                {...vt(id, "d")}
                className={`min-w-0 flex-[1_1_0] overflow-hidden text-ellipsis text-ink-3 before:me-[8px] before:content-['·'] ${dim ? "opacity-60" : ""}`}
              >
                {t(`onboarding.model.shorts.${ROLE_KEYS[role]}`)}
              </span>
            )}
          </span>
        </span>
        {columns(row)}
        {line && thinLine("inset-x-5 bottom-[6px]")}
      </button>
    );
  };

  /** One line: role, few words (or the name), one grade, speed and size. */
  const oneLine = (row: ModelRow, role: PickRole | "extra") => {
    const id = row.model.id;
    const line = lineFor(id);
    const overall = grading.overall(row.model);
    return (
      <button
        key={id}
        type="button"
        role="radio"
        aria-checked="false"
        {...vt(id)}
        data-vt-row=""
        onClick={() => choose(id, true)}
        className={`relative flex w-full min-w-0 cursor-pointer items-center gap-[10px] whitespace-nowrap rounded-card border-[1.5px] border-line bg-background px-5 text-start text-small text-text hover:border-ink-3 hover:bg-wash [view-transition-class:mf-row_mf-box] ${line ? "pt-[8px] pb-[15px]" : "py-[8px]"}`}
      >
        {mark(id, false)}
        <b {...vt(id, "r")} className={`${roleBase} text-label text-text`}>
          {t(`modelTable.roles.${ROLE_KEYS[role]}`)}
        </b>
        {capabilityTags(row.model)}
        <span
          {...vt(id, "d")}
          className={`min-w-0 flex-[1_1_0] overflow-hidden text-ellipsis text-ink-3 ${row.missing.length ? "opacity-60" : ""}`}
        >
          {role === "extra"
            ? row.model.name
            : t(`onboarding.model.shorts.${ROLE_KEYS[role]}`)}
        </span>
        <span className="ms-auto flex shrink-0 items-center gap-[10px]">
          <span {...vt(id, "g")} className="flex items-center gap-[10px]">
            <GradeTip grading={grading} model={row.model} langs={langs}>
              <span className="inline-grid">
                <GradeChip letter={overall.letter} label={overall.label} />
              </span>
            </GradeTip>
            <SpeedTip
              grading={grading}
              model={row.model}
              streaming={previews(row.model)}
            >
              <span className="inline-grid">
                <SpeedBars level={speedLevel(row.wait)} />
              </span>
            </SpeedTip>
          </span>
          <span {...vt(id, "s")} className={`${sizeClass} ms-0`}>
            {sizeOf(row.model)}
          </span>
        </span>
        {line && thinLine("inset-x-5 bottom-[6px]")}
      </button>
    );
  };

  /* ---------------- the footer ---------------- */

  // Nothing to download for a model on disk, or the one already downloading,
  // or when Built-in macOS Voice Only (0 MB) is chosen.
  const selectedState = isNativeSelected
    ? "ready"
    : models.downloadState(selected).state;
  const continuing =
    isNativeSelected ||
    selectedState === "ready" ||
    (downloadId === selectedId && selectedState !== "failed");
  // Switching mid-download pauses it (the partial stays on disk), so no dialog.
  const switching =
    !isNativeSelected &&
    loading &&
    loading.id !== selectedId &&
    loadingState.state === "downloading"
      ? { name: loading.name, percent: words.percent(loadingState) }
      : null;
  const primary = (
    <Button
      variant="primary"
      size="lg"
      data-testid="onboarding-model-continue"
      onClick={() => onChoose(selectedId)}
    >
      {isMac
        ? continuing
          ? t("onboarding.shell.continue")
          : t("onboarding.model.consumer.startInstantlyCta", {
              defaultValue: "Start instantly",
            })
        : continuing
          ? t("onboarding.shell.continue")
          : t(
              switching
                ? "onboarding.model.downloadInstead"
                : "onboarding.model.download",
              { size: selected ? sizeOf(selected) : "" },
            )}
    </Button>
  );
  const hint = switching ? (
    <div className="text-start leading-[1.35]">
      <div className="truncate font-medium text-text">
        {t("onboarding.model.pauses", switching)}
      </div>
      <div className="truncate text-label">
        {t("onboarding.model.resumeLater")}
      </div>
    </div>
  ) : isNativeSelected ? (
    t("onboarding.model.consumer.nativeHint", {
      defaultValue: "0 MB download \u00b7 100% on-device macOS voice",
    })
  ) : isMac ? (
    continuing
      ? ""
      : t("onboarding.model.consumer.hdBackgroundHint", {
          defaultValue:
            "Dictate right away \u00b7 HD voice prepares quietly in the background",
        })
  ) : continuing ? (
    ""
  ) : (
    t("onboarding.model.hint")
  );

  const title = <span data-vt="mf-title">{t("onboarding.model.title")}</span>;
  const sub = <span data-vt="mf-sub">{t("onboarding.model.subtitle")}</span>;

  /* ---------------- the picks ---------------- */

  if (!showAll) {
    const extra = extraId ? rowFor(extraId) : undefined;
    const items = picks.map((p) => {
      const row = rowFor(p.model.id);
      if (!row) return null;
      if (p.model.id === selectedId) return fullCard(row, p.role);
      return extra ? oneLine(row, p.role) : card(row, p.role);
    });
    if (extra)
      items.push(
        extra.model.id === selectedId
          ? fullCard(extra, "extra")
          : oneLine(extra, "extra"),
      );

    const goalCard = (
      goalKey: string,
      goalModel: CatalogModel | undefined,
      icon: React.ReactNode,
      cardTitle: string,
      cardDesc: string,
    ) => {
      if (!goalModel) return null;
      const active = selectedId === goalModel.id;
      const line = lineFor(goalModel.id);
      return (
        <button
          key={goalKey}
          type="button"
          aria-pressed={active}
          onClick={() => choose(goalModel.id, false)}
          className={`relative flex min-w-0 cursor-pointer flex-col items-start gap-[6px] rounded-panel border-[1.5px] bg-background px-[16px] text-start transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary ${
            active
              ? "border-plum-pick"
              : "border-line-strong hover:border-ink-3 hover:bg-wash"
          } ${line ? "pt-[14px] pb-[18px]" : "py-[14px]"}`}
        >
          <span className="flex w-full items-center justify-between gap-[8px]">
            <span
              className={`grid size-[32px] shrink-0 place-items-center rounded-[9px] ${
                active
                  ? "bg-background-ui/12 text-background-ui"
                  : "bg-wash text-ink-2"
              }`}
            >
              {icon}
            </span>
            <Radio checked={active} />
          </span>
          <b className="mt-[2px] font-display text-name font-[650] leading-[1.25] tracking-[-0.01em] text-text">
            {cardTitle}
          </b>
          <span className="text-small leading-[1.4] text-ink-2 text-pretty">
            {cardDesc}
          </span>
          <span className="mt-auto inline-flex max-w-full items-center pt-[4px]">
            <Tag className="inline-flex max-w-full items-center gap-[5px]">
              <span className="truncate">{goalModel.name}</span>
              <span aria-hidden="true" className="text-ink-3">
                ·
              </span>
              <span className="shrink-0 tabular-nums text-ink-3">
                {sizeOf(goalModel)}
              </span>
            </Tag>
          </span>
          {line && thinLine("inset-x-4 bottom-[6px]")}
        </button>
      );
    };

    const hasBothGoals = !!(englishGoalModel && multilingualGoalModel);
    const hdTargetId = autoHdModel?.id ?? MACOS_NATIVE_SPEECH_ID;
    const isHdConsumerActive =
      !isNativeSelected &&
      (!advancedOpen || selectedId === hdTargetId);
    const hdLine = autoHdModel ? lineFor(autoHdModel.id) : false;

    return (
      <OnboardingShell
        progress={progress}
        onBack={onBack}
        hint={hint}
        primary={primary}
      >
        <ScreenMain
          ref={rootRef}
          data-model-root=""
          layout={advancedOpen ? "top" : "center"}
          align="stretch"
          header={<ScreenHeader title={title} sub={sub} />}
        >
          <div
            data-model-scroll={advancedOpen ? "" : undefined}
            className={`flex w-full flex-col ${
              advancedOpen ? "min-h-0 flex-1 overflow-y-auto pb-[4px]" : ""
            }`}
          >
            {isMac ? (
              <div
                data-testid="macos-zero-wait-consumer-cards"
                className="w-full flex-none text-start"
              >
                <h2 className="m-0 mb-[8px] text-label font-[650] uppercase tracking-[0.04em] text-ink-3">
                  {t("onboarding.model.consumer.heading", {
                    defaultValue: "Start dictating right away — zero waiting",
                  })}
                </h2>
                <div className="grid grid-cols-2 gap-[10px]">
                  {/* Option 1: Start Instantly + Prepare HD Offline Voice in Background (Recommended) */}
                  <button
                    type="button"
                    data-testid="consumer-option-instant-hd"
                    aria-pressed={isHdConsumerActive}
                    onClick={() => choose(hdTargetId, false)}
                    className={`relative flex min-w-0 cursor-pointer flex-col items-start gap-[6px] rounded-panel border-[1.5px] bg-background px-[16px] text-start transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary ${
                      isHdConsumerActive
                        ? "border-plum-pick"
                        : "border-line-strong hover:border-ink-3 hover:bg-wash"
                    } ${hdLine ? "pt-[14px] pb-[18px]" : "py-[14px]"}`}
                  >
                    <span className="flex w-full items-center justify-between gap-[8px]">
                      <span className="inline-flex items-center gap-[8px]">
                        <span
                          className={`grid size-[32px] shrink-0 place-items-center rounded-[9px] ${
                            isHdConsumerActive
                              ? "bg-background-ui/12 text-background-ui"
                              : "bg-wash text-ink-2"
                          }`}
                        >
                          <Zap
                            size={16}
                            strokeWidth={2.2}
                            aria-hidden="true"
                          />
                        </span>
                        <Tag className="font-semibold text-logo-primary">
                          {t("onboarding.model.consumer.recommendedBadge", {
                            defaultValue: "Recommended",
                          })}
                        </Tag>
                      </span>
                      <Radio checked={isHdConsumerActive} />
                    </span>
                    <b className="mt-[2px] font-display text-name font-[650] leading-[1.25] tracking-[-0.01em] text-text">
                      {t("onboarding.model.consumer.instantHdTitle", {
                        defaultValue:
                          "Start Instantly + Prepare HD Offline Voice in Background",
                      })}
                    </b>
                    <span className="text-small leading-[1.4] text-ink-2 text-pretty">
                      {isEnglishOnly
                        ? t("onboarding.model.consumer.instantHdDescEnglish", {
                            defaultValue:
                              "Try \u2325 Space right away with built-in macOS voice while lowyap quietly prepares your high-accuracy English voice pack in the background.",
                          })
                        : t(
                            "onboarding.model.consumer.instantHdDescMultilingual",
                            {
                              defaultValue:
                                "Try \u2325 Space right away with built-in macOS voice while lowyap quietly prepares your 100+ language HD voice pack in the background.",
                            },
                          )}
                    </span>
                    <span className="mt-auto inline-flex max-w-full items-center pt-[4px]">
                      <Tag className="inline-flex max-w-full items-center gap-[5px]">
                        <span className="truncate">
                          {t("onboarding.model.consumer.instantHdTag", {
                            defaultValue: "0s wait \u00b7 Auto-upgrades when ready",
                          })}
                        </span>
                      </Tag>
                    </span>
                    {hdLine && thinLine("inset-x-4 bottom-[6px]")}
                  </button>

                  {/* Option 2: Built-in macOS Voice Only (0 MB download) */}
                  <button
                    type="button"
                    data-testid="consumer-option-macos-native"
                    aria-pressed={isNativeSelected}
                    onClick={() => choose(MACOS_NATIVE_SPEECH_ID, false)}
                    className={`relative flex min-w-0 cursor-pointer flex-col items-start gap-[6px] rounded-panel border-[1.5px] bg-background px-[16px] py-[14px] text-start transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary ${
                      isNativeSelected
                        ? "border-plum-pick"
                        : "border-line-strong hover:border-ink-3 hover:bg-wash"
                    }`}
                  >
                    <span className="flex w-full items-center justify-between gap-[8px]">
                      <span className="inline-flex items-center gap-[8px]">
                        <span
                          className={`grid size-[32px] shrink-0 place-items-center rounded-[9px] ${
                            isNativeSelected
                              ? "bg-background-ui/12 text-background-ui"
                              : "bg-wash text-ink-2"
                          }`}
                        >
                          <Cpu
                            size={16}
                            strokeWidth={2.1}
                            aria-hidden="true"
                          />
                        </span>
                        <Tag className="font-mono">
                          {t("onboarding.model.consumer.zeroMbBadge", {
                            defaultValue: "0 MB download",
                          })}
                        </Tag>
                      </span>
                      <Radio checked={isNativeSelected} />
                    </span>
                    <b className="mt-[2px] font-display text-name font-[650] leading-[1.25] tracking-[-0.01em] text-text">
                      {t("onboarding.model.consumer.nativeOnlyTitle", {
                        defaultValue:
                          "Built-in macOS Voice Only (0 MB download)",
                      })}
                    </b>
                    <span className="text-small leading-[1.4] text-ink-2 text-pretty">
                      {t("onboarding.model.consumer.nativeOnlyDesc", {
                        defaultValue:
                          "Uses Apple\u2019s built-in on-device speech engine with zero extra disk space and zero downloads. 100% private on your Mac.",
                      })}
                    </span>
                    <span className="mt-auto inline-flex max-w-full items-center pt-[4px]">
                      <Tag className="inline-flex max-w-full items-center gap-[5px]">
                        <span className="truncate">
                          {t("onboarding.model.consumer.nativeOnlyTag", {
                            defaultValue: "0 MB disk space \u00b7 100% on-device",
                          })}
                        </span>
                      </Tag>
                    </span>
                  </button>
                </div>
              </div>
            ) : (
              <div className="w-full flex-none text-start">
                <h2 className="m-0 mb-[8px] text-label font-[650] uppercase tracking-[0.04em] text-ink-3">
                  {t("onboarding.model.goals.heading")}
                </h2>
                <div
                  className={`grid gap-[10px] ${
                    hasBothGoals ? "grid-cols-2" : "grid-cols-1"
                  }`}
                >
                  {goalCard(
                    "english",
                    englishGoalModel,
                    <Zap size={16} strokeWidth={2.2} aria-hidden="true" />,
                    t("onboarding.model.goals.englishTitle"),
                    t("onboarding.model.goals.englishDesc"),
                  )}
                  {goalCard(
                    "multilingual",
                    multilingualGoalModel,
                    <Globe size={16} strokeWidth={2.1} aria-hidden="true" />,
                    t("onboarding.model.goals.multilingualTitle"),
                    t("onboarding.model.goals.multilingualDesc"),
                  )}
                </div>
              </div>
            )}
            <button
              type="button"
              data-testid="onboarding-model-advanced-toggle"
              aria-expanded={advancedOpen}
              aria-controls="onboarding-model-advanced"
              onClick={() => setAdvancedOpen((prev) => !prev)}
              className="mt-[10px] -ms-[8px] inline-flex flex-none cursor-pointer items-center gap-[6px] self-start rounded-lg px-[8px] py-[4px] text-small font-medium text-ink-2 transition-colors hover:bg-wash hover:text-text focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary"
            >
              <span>
                {advancedOpen
                  ? t("onboarding.model.consumer.advancedHide", {
                      defaultValue: "Hide advanced voice engines",
                    })
                  : t("onboarding.model.consumer.advancedShow", {
                      count: models.offered.length,
                      defaultValue:
                        "Choose a specific voice engine (Advanced)",
                    })}
              </span>
              <ChevronDown
                size={14}
                aria-hidden="true"
                className={`transition-transform duration-200 ${
                  advancedOpen ? "rotate-180" : ""
                }`}
              />
            </button>
            {advancedOpen && (
              <div
                id="onboarding-model-advanced"
                className="mt-[8px] flex w-full flex-none flex-col"
              >
                {isMac && hasBothGoals && (
                  <div className="mb-[8px] grid grid-cols-2 gap-[10px] text-start">
                    {goalCard(
                      "english",
                      englishGoalModel,
                      <Zap size={16} strokeWidth={2.2} aria-hidden="true" />,
                      t("onboarding.model.goals.englishTitle"),
                      t("onboarding.model.goals.englishDesc"),
                    )}
                    {goalCard(
                      "multilingual",
                      multilingualGoalModel,
                      <Globe size={16} strokeWidth={2.1} aria-hidden="true" />,
                      t("onboarding.model.goals.multilingualTitle"),
                      t("onboarding.model.goals.multilingualDesc"),
                    )}
                  </div>
                )}
                <div
                  role="radiogroup"
                  aria-label={t("onboarding.model.label")}
                  className="flex w-full flex-none flex-col gap-[8px] text-start"
                >
                  {items}
                </div>
                <button
                  type="button"
                  onClick={() => open()}
                  className="mt-[8px] flex h-[38px] flex-none cursor-pointer items-center gap-[8px] self-stretch rounded-card border-[1.5px] border-dashed border-line-strong px-[20px] text-start text-small font-semibold text-ink-2 hover:border-solid hover:bg-wash hover:text-text"
                >
                  {t("onboarding.model.showAll", {
                    count: models.offered.length,
                  })}
                  <Chevron />
                </button>
              </div>
            )}
          </div>
        </ScreenMain>
      </OnboardingShell>
    );
  }

  /* ---------------- every model ---------------- */

  const kept = (rows: ModelRow[]) => entriesFor(rows).filter(matches);
  // Picks, then the rest by value; models missing a language last.
  const sections: TableSection[] = [
    {
      key: "fits",
      label: t("onboarding.model.model"),
      rows: [...kept(groups.picks), ...kept(groups.others)],
    },
    {
      key: "partial",
      label: t("modelTable.missingLanguage"),
      rows: kept(groups.partial),
    },
  ].filter((s) => s.rows.length);

  const tableRow = (entry: TableModel) => {
    const id = entry.id;
    const on = id === selectedId;
    return (
      <ModelTableRow
        key={id}
        entry={entry}
        selected={on}
        mark={<Radio checked={on} />}
        download={
          lineFor(id)
            ? {
                state:
                  loadingState.state === "checking"
                    ? "checking"
                    : "downloading",
                percent: lineValue * 100,
              }
            : null
        }
        onActivate={() => choose(id, false)}
        vt={(part) => vt(id, part)}
      />
    );
  };

  return (
    <OnboardingShell
      progress={progress}
      onBack={close}
      hint={hint}
      primary={primary}
    >
      <ScreenMain
        ref={rootRef}
        data-model-root=""
        layout="top"
        align="stretch"
        header={<ScreenHeader title={title} sub={sub} />}
      >
        <ModelTable
          grading={grading}
          filters={filters}
          setFilters={setFilters}
          sections={sections}
          renderRow={tableRow}
          onEscape={close}
          focusOnMount
        />
      </ScreenMain>
    </OnboardingShell>
  );
};
