import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import {
  commands,
  type EdgeDockMode,
  type EdgeDockPosition,
  type EdgeDockPresetItem,
  type EdgeDockUiState,
} from "@/bindings";
import {
  SESSION_BUS_EVENT,
  type SessionBusSnapshot,
  fetchSessionBusSnapshot,
} from "@/lib/sessionBus";
import "./EdgeDock.css";

type HoveredAction = "dictate" | "meeting" | "transform" | null;
type DockMenuKind = "transform" | "context";

const BUILTIN_POLISH_ID = "default_improve_transcriptions";
const BUILTIN_PROMPT_ENGINEER_ID = "builtin_prompt_engineer";
const LEGACY_PROMPT_ENGINEER_ID = "default_prompt_engineer";
const LEAVE_COLLAPSE_MS = 75;

const hasTauri =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

function nearestEdgeClient(
  clientX: number,
  clientY: number,
  width: number,
  height: number,
): EdgeDockPosition {
  const w = Math.max(width, 1);
  const h = Math.max(height, 1);
  const nx = Math.min(1, Math.max(0, clientX / w));
  const ny = Math.min(1, Math.max(0, clientY / h));
  const distLeft = Math.hypot(nx - 0.02, ny - 0.5);
  const distRight = Math.hypot(nx - 0.98, ny - 0.5);
  const distBottom = Math.sqrt((nx - 0.5) ** 2 * 0.65 + (ny - 0.96) ** 2);
  if (distBottom <= distLeft && distBottom <= distRight) {
    return "bottom";
  }
  return distRight < distLeft ? "right" : "left";
}

function initialBrowserFallbackState(): EdgeDockUiState {
  const params = new URLSearchParams(
    typeof window !== "undefined" ? window.location.search : "",
  );
  const rawEdge = params.get("edge");
  const position: EdgeDockPosition =
    rawEdge === "left" || rawEdge === "right" ? rawEdge : "bottom";
  const rawMode = params.get("mode");
  const mode: EdgeDockMode =
    rawMode === "expanded" || rawMode === "dragging" ? rawMode : "collapsed";
  return {
    enabled: true,
    snoozed: false,
    position,
    mode,
    menu_open: false,
    preview_position: position,
    dictate_shortcut: "⌥Space",
    meeting_shortcut: "",
    transform_shortcut: "⌥1",
    auto_apply_enabled: true,
    selected_preset_id: BUILTIN_POLISH_ID,
    active_preset_title: "Polish",
    presets: [
      { id: BUILTIN_POLISH_ID, name: "Polish" },
      { id: BUILTIN_PROMPT_ENGINEER_ID, name: "Prompt Engineer" },
    ],
    cursor_rel_x: null,
    cursor_rel_y: null,
    theme: "dark",
  };
}

/** Intuitive macOS Microphone glyph for Dictate. */
const DictateMicGlyph: React.FC = () => (
  <svg
    className="ed-icon"
    width="15"
    height="15"
    viewBox="0 0 16 16"
    fill="none"
    aria-hidden="true"
  >
    <rect
      x="5.6"
      y="1.8"
      width="4.8"
      height="7.6"
      rx="2.4"
      stroke="currentColor"
      strokeWidth="1.45"
    />
    <path
      d="M3.5 7.3C3.5 9.78 5.52 11.8 8 11.8C10.48 11.8 12.5 9.78 12.5 7.3M8 11.8V14.2M5.8 14.2H10.2"
      stroke="currentColor"
      strokeWidth="1.45"
      strokeLinecap="round"
      strokeLinejoin="round"
    />
  </svg>
);

/** Intuitive concentric record glyph for bot-free Meeting Notetaker. */
const MeetingRecordGlyph: React.FC = () => (
  <svg
    className="ed-icon"
    width="15"
    height="15"
    viewBox="0 0 16 16"
    fill="none"
    aria-hidden="true"
  >
    <circle
      cx="8"
      cy="8"
      r="5.6"
      stroke="currentColor"
      strokeWidth="1.4"
      strokeOpacity="0.88"
    />
    <circle cx="8" cy="8" r="2.35" fill="#FF5A4E" />
  </svg>
);

/** Intuitive magic wand / sparkles glyph for Transform & Polish. */
const TransformWandGlyph: React.FC = () => (
  <svg
    className="ed-icon"
    width="15"
    height="15"
    viewBox="0 0 16 16"
    fill="none"
    aria-hidden="true"
  >
    <path
      d="M9.8 2.2L10.45 3.95L12.2 4.6L10.45 5.25L9.8 7L9.15 5.25L7.4 4.6L9.15 3.95L9.8 2.2Z"
      fill="currentColor"
    />
    <path
      d="M4.2 8.4L4.65 9.55L5.8 10L4.65 10.45L4.2 11.6L3.75 10.45L2.6 10L3.75 9.55L4.2 8.4Z"
      fill="currentColor"
    />
    <path
      d="M3.1 13.3L9.1 7.3"
      stroke="currentColor"
      strokeWidth="1.45"
      strokeLinecap="round"
    />
  </svg>
);

export const EdgeDock: React.FC = () => {
  const { t } = useTranslation();
  const [state, setState] = useState<EdgeDockUiState>(
    initialBrowserFallbackState,
  );
  const [hoveredAction, setHoveredAction] = useState<HoveredAction>(null);
  const [tipOffsetPx, setTipOffsetPx] = useState<number>(0);
  const [tipGliding, setTipGliding] = useState<boolean>(false);
  const [meetingActive, setMeetingActive] = useState<boolean>(false);
  const [menuKind, setMenuKind] = useState<DockMenuKind>("transform");
  const [localCursor, setLocalCursor] = useState<{
    x: number;
    y: number;
  } | null>(null);

  const dragOriginRef = useRef<{ x: number; y: number } | null>(null);
  const dragStartedRef = useRef(false);
  const leaveTimerRef = useRef<number | null>(null);
  const suppressHoverUntilLeaveRef = useRef(false);
  const prevHoveredRef = useRef<HoveredAction>(null);
  const clusterRef = useRef<HTMLDivElement | null>(null);
  const clusterRowRef = useRef<HTMLDivElement | null>(null);
  const dictateBtnRef = useRef<HTMLButtonElement | null>(null);
  const meetingBtnRef = useRef<HTMLButtonElement | null>(null);
  const transformBtnRef = useRef<HTMLButtonElement | null>(null);
  const readoutRef = useRef<HTMLDivElement | null>(null);
  const popoverRef = useRef<HTMLDivElement | null>(null);

  const resolveHoveredActionAtPoint = useCallback(
    (clientX: number, clientY: number): HoveredAction => {
      if (clientX < 0 || clientY < 0) return null;
      const entries: Array<[Exclude<HoveredAction, null>, HTMLButtonElement | null]> = [
        ["dictate", dictateBtnRef.current],
        ["meeting", meetingBtnRef.current],
        ["transform", transformBtnRef.current],
      ];
      const pad = 4.5;
      let best: Exclude<HoveredAction, null> | null = null;
      let bestDist = Number.POSITIVE_INFINITY;
      for (const [action, el] of entries) {
        if (!el) continue;
        const r = el.getBoundingClientRect();
        if (r.width <= 0 || r.height <= 0) continue;
        if (
          clientX >= r.left - pad &&
          clientX <= r.right + pad &&
          clientY >= r.top - pad &&
          clientY <= r.bottom + pad
        ) {
          const cx = (r.left + r.right) * 0.5;
          const cy = (r.top + r.bottom) * 0.5;
          const d = Math.hypot(clientX - cx, clientY - cy);
          if (d < bestDist) {
            bestDist = d;
            best = action;
          }
        }
      }
      return best;
    },
    [],
  );

  const clearHoverTimers = useCallback(() => {
    if (leaveTimerRef.current !== null) {
      window.clearTimeout(leaveTimerRef.current);
      leaveTimerRef.current = null;
    }
  }, []);

  const refreshState = useCallback(async () => {
    if (!hasTauri) return;
    try {
      const next = await commands.getEdgeDockState();
      setState(next);
    } catch {
      // Ignore transient startup race before manager init.
    }
  }, []);

  useEffect(() => {
    if (!hasTauri) return;
    void refreshState();

    const unlisteners: Array<Promise<() => void>> = [
      listen<EdgeDockUiState>("edge-dock-state", (ev) => {
        setState(ev.payload);
        if (ev.payload.mode !== "expanded" || ev.payload.menu_open) {
          setHoveredAction(null);
        }
      }),
      listen<{ x: number; y: number }>("edge-dock-cursor", (ev) => {
        const { x, y } = ev.payload;
        const next = resolveHoveredActionAtPoint(x, y);
        setHoveredAction((curr) => (curr === next ? curr : next));
      }),
      listen("transform-settings-changed", () => {
        void refreshState();
      }),
      listen("flow-settings-changed", () => {
        void refreshState();
      }),
      listen<SessionBusSnapshot>(SESSION_BUS_EVENT, (ev) => {
        setMeetingActive(ev.payload.meeting.phase !== "idle");
      }),
    ];

    void fetchSessionBusSnapshot()
      .then((snap) => {
        if (snap) setMeetingActive(snap.meeting.phase !== "idle");
      })
      .catch(() => {});

    return () => {
      for (const p of unlisteners) {
        void p.then((fn) => fn());
      }
    };
  }, [refreshState, resolveHoveredActionAtPoint]);

  useEffect(() => () => clearHoverTimers(), [clearHoverTimers]);

  const setExpanded = useCallback(
    (expanded: boolean, latchUntilPointerLeaves = false) => {
      clearHoverTimers();
      if (!expanded) {
        setHoveredAction(null);
      }
      suppressHoverUntilLeaveRef.current = !expanded && latchUntilPointerLeaves;
      if (hasTauri) {
        void commands.setEdgeDockExpanded(expanded);
      } else {
        setState((prev) =>
          prev.mode === "dragging"
            ? prev
            : {
                ...prev,
                mode: expanded ? "expanded" : "collapsed",
                menu_open: expanded ? prev.menu_open : false,
              },
        );
      }
    },
    [clearHoverTimers],
  );

  const setMenuOpen = useCallback(
    (open: boolean) => {
      clearHoverTimers();
      if (hasTauri) {
        void commands.setEdgeDockMenuOpen(open);
      } else {
        setState((prev) => ({
          ...prev,
          mode: open ? "expanded" : prev.mode,
          menu_open: open,
        }));
      }
    },
    [clearHoverTimers],
  );

  useEffect(() => {
    const onKeyDown = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") {
        setMenuOpen(false);
        setExpanded(false, true);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [setExpanded, setMenuOpen]);

  useEffect(() => {
    if (!state.menu_open) return;
    const onPointerDownOutside = (ev: PointerEvent) => {
      const target = ev.target as Node | null;
      if (!target) return;
      if (
        popoverRef.current?.contains(target) ||
        clusterRef.current?.contains(target)
      ) {
        return;
      }
      setMenuOpen(false);
      setExpanded(false);
    };
    window.addEventListener("pointerdown", onPointerDownOutside);
    return () =>
      window.removeEventListener("pointerdown", onPointerDownOutside);
  }, [state.menu_open, setExpanded, setMenuOpen]);

  const startDrag = useCallback(
    (clientX: number, clientY: number) => {
      clearHoverTimers();
      dragStartedRef.current = true;
      setLocalCursor({ x: clientX, y: clientY });
      if (hasTauri) {
        void commands.beginEdgeDockDrag();
      } else {
        setState((prev) => ({
          ...prev,
          mode: "dragging",
          menu_open: false,
          preview_position: prev.position,
        }));
      }
    },
    [clearHoverTimers],
  );

  const finishDrag = useCallback(
    (edge: EdgeDockPosition) => {
      clearHoverTimers();
      dragOriginRef.current = null;
      dragStartedRef.current = false;
      setLocalCursor(null);
      if (hasTauri) {
        void commands.finishEdgeDockDrag(edge);
      } else {
        setState((prev) => ({
          ...prev,
          position: edge,
          preview_position: edge,
          menu_open: false,
          mode: "collapsed",
        }));
      }
    },
    [clearHoverTimers],
  );

  const handlePointerDownForDrag = useCallback(
    (e: React.PointerEvent<HTMLElement>) => {
      if (e.button !== 0) return;
      dragOriginRef.current = { x: e.clientX, y: e.clientY };
      dragStartedRef.current = false;
    },
    [],
  );

  useEffect(() => {
    const onWindowPointerMove = (e: PointerEvent) => {
      const origin = dragOriginRef.current;
      if (!origin) return;
      if ((e.buttons & 1) === 0) {
        dragOriginRef.current = null;
        return;
      }
      const dist = Math.hypot(e.clientX - origin.x, e.clientY - origin.y);
      if (!dragStartedRef.current && dist >= 6) {
        startDrag(e.clientX, e.clientY);
      }
    };
    const onWindowPointerUp = () => {
      dragOriginRef.current = null;
    };
    window.addEventListener("pointermove", onWindowPointerMove);
    window.addEventListener("pointerup", onWindowPointerUp);
    return () => {
      window.removeEventListener("pointermove", onWindowPointerMove);
      window.removeEventListener("pointerup", onWindowPointerUp);
    };
  }, [startDrag]);

  const formatPresetTitle = useCallback(
    (preset: EdgeDockPresetItem) => {
      if (preset.id === BUILTIN_POLISH_ID) {
        return t("overlay.transformPresetPolish", {
          defaultValue: preset.name,
        });
      }
      if (
        preset.id === BUILTIN_PROMPT_ENGINEER_ID ||
        preset.id === LEGACY_PROMPT_ENGINEER_ID
      ) {
        return t("overlay.transformPresetPromptEngineer", {
          defaultValue: preset.name,
        });
      }
      return preset.name;
    },
    [t],
  );

  const handleToggleAutoApply = useCallback(async () => {
    const next = !state.auto_apply_enabled;
    setState((prev) => ({ ...prev, auto_apply_enabled: next }));
    if (hasTauri) {
      try {
        await commands.changePostProcessEnabledSetting(next);
      } catch {
        void refreshState();
      }
    }
  }, [refreshState, state.auto_apply_enabled]);

  const handleSelectPreset = useCallback(
    async (preset: EdgeDockPresetItem) => {
      const switchedStyle = preset.id !== state.selected_preset_id;
      setState((prev) => ({
        ...prev,
        selected_preset_id: preset.id,
        active_preset_title: preset.name,
        auto_apply_enabled: true,
      }));
      if (hasTauri) {
        try {
          await commands.setPostProcessSelectedPrompt(preset.id);
          if (!state.auto_apply_enabled) {
            await commands.changePostProcessEnabledSetting(true);
          }
          // Switching styles (e.g. Prompt Engineer -> Polish) immediately re-transforms
          // any active selection or recent dictation using TransformLineage raw_source.
          if (switchedStyle) {
            setMenuOpen(false);
            void commands.triggerDockTransform(preset.id);
          }
        } catch {
          void refreshState();
        }
      }
    },
    [
      refreshState,
      setMenuOpen,
      state.auto_apply_enabled,
      state.selected_preset_id,
    ],
  );

  const handleRunTransformNow = useCallback(
    async (promptId: string | null = null) => {
      setMenuOpen(false);
      if (hasTauri) {
        void commands.triggerDockTransform(promptId);
      } else {
        setState((prev) => ({
          ...prev,
          menu_open: false,
          mode: "collapsed",
        }));
      }
    },
    [setMenuOpen],
  );

  const handleOpenConfigure = useCallback(async () => {
    setMenuOpen(false);
    setExpanded(false);
    if (hasTauri) {
      try {
        await commands.openTransformsWorkspace();
      } catch {
        // Ignore in isolated test harnesses without Studio window.
      }
    }
  }, [setExpanded, setMenuOpen]);

  const handleSnoozeOneHour = useCallback(() => {
    clearHoverTimers();
    if (hasTauri) {
      void commands.snoozeEdgeDock(60);
    } else {
      setState((prev) => ({
        ...prev,
        snoozed: true,
        menu_open: false,
        mode: "collapsed",
      }));
    }
  }, [clearHoverTimers]);

  const handleHideDock = useCallback(() => {
    clearHoverTimers();
    if (hasTauri) {
      void commands.hideEdgeDock();
    } else {
      setState((prev) => ({
        ...prev,
        enabled: false,
        menu_open: false,
        mode: "collapsed",
      }));
    }
  }, [clearHoverTimers]);

  const activePreset =
    state.presets.find((p) => p.id === state.selected_preset_id) ??
    state.presets[0] ?? {
      id: BUILTIN_POLISH_ID,
      name: state.active_preset_title || "Polish",
    };
  const activePresetDisplay = formatPresetTitle(activePreset);

  const readoutLabel =
    hoveredAction === "meeting"
      ? meetingActive
        ? t("edgeDock.actions.openLiveMeeting", {
            defaultValue: "Open live meeting",
          })
        : t("edgeDock.actions.meeting")
      : hoveredAction === "transform"
        ? `${t("edgeDock.actions.transform")} · ${activePresetDisplay}`
        : t("edgeDock.actions.dictate");

  const readoutKbd =
    hoveredAction === "meeting"
      ? meetingActive
        ? t("edgeDock.liveBadge", { defaultValue: "LIVE" })
        : state.meeting_shortcut || t("edgeDock.noBotBadge")
      : hoveredAction === "transform"
        ? state.transform_shortcut
        : state.dictate_shortcut;

  const isExpanded = state.mode === "expanded";

  // Anchor the floating tooltip directly over the hovered icon and clamp within the 264×264 canvas
  // so sliding across Dictate -> Meeting -> Transform glides smoothly without clipping.
  React.useLayoutEffect(() => {
    if (!hoveredAction || !isExpanded || state.menu_open || !state.enabled || state.snoozed) {
      prevHoveredRef.current = null;
      setTipGliding(false);
      return;
    }
    const wrap = clusterRef.current;
    const target =
      hoveredAction === "dictate"
        ? dictateBtnRef.current
        : hoveredAction === "meeting"
          ? meetingBtnRef.current
          : transformBtnRef.current;
    if (!wrap || !target) return;

    const wrapRect = wrap.getBoundingClientRect();
    const targetRect = target.getBoundingClientRect();
    const hadPrior = prevHoveredRef.current !== null && prevHoveredRef.current !== hoveredAction;
    prevHoveredRef.current = hoveredAction;
    setTipGliding(hadPrior);

    let nextOffset = 0;
    if (state.position === "bottom") {
      const rawOffset =
        targetRect.left + targetRect.width * 0.5 - (wrapRect.left + wrapRect.width * 0.5);
      const tipW = readoutRef.current?.offsetWidth ?? 136;
      const canvasW = typeof window !== "undefined" ? window.innerWidth : 264;
      const maxShift = Math.max(0, canvasW * 0.5 - tipW * 0.5 - 6);
      nextOffset = Math.max(-maxShift, Math.min(maxShift, rawOffset));
    } else {
      const rawOffset =
        targetRect.top + targetRect.height * 0.5 - (wrapRect.top + wrapRect.height * 0.5);
      const tipH = readoutRef.current?.offsetHeight ?? 26;
      const canvasH = typeof window !== "undefined" ? window.innerHeight : 264;
      const maxShift = Math.max(0, canvasH * 0.5 - tipH * 0.5 - 6);
      nextOffset = Math.max(-maxShift, Math.min(maxShift, rawOffset));
    }
    const rounded = Math.round(nextOffset * 10) / 10;
    setTipOffsetPx(rounded);
    readoutRef.current?.style.setProperty("--ed-tip-offset", `${rounded}px`);
  }, [
    hoveredAction,
    isExpanded,
    state.menu_open,
    state.enabled,
    state.snoozed,
    state.position,
    readoutLabel,
    readoutKbd,
  ]);

  if (!state.enabled || state.snoozed) {
    return null;
  }

  if (state.mode === "dragging") {
    const activePreview = state.preview_position;
    const puckX =
      localCursor?.x ??
      state.cursor_rel_x ??
      (typeof window !== "undefined" ? window.innerWidth / 2 : 132);
    const puckY =
      localCursor?.y ??
      state.cursor_rel_y ??
      (typeof window !== "undefined" ? window.innerHeight / 2 : 132);

    return (
      <div
        className="ed-stage"
        data-testid="edge-dock-stage"
        data-mode="dragging"
        data-edge={state.position}
      >
        <div
          className="ed-drag-canvas"
          data-testid="edge-dock-drag-canvas"
          onPointerMove={(e) => {
            setLocalCursor({ x: e.clientX, y: e.clientY });
            const nextEdge = nearestEdgeClient(
              e.clientX,
              e.clientY,
              window.innerWidth,
              window.innerHeight,
            );
            setState((prev) =>
              prev.preview_position === nextEdge
                ? prev
                : { ...prev, preview_position: nextEdge },
            );
          }}
          onPointerUp={(e) => {
            const snapped = nearestEdgeClient(
              e.clientX,
              e.clientY,
              window.innerWidth,
              window.innerHeight,
            );
            finishDrag(snapped);
          }}
        >
          <div
            className="ed-snap-zone"
            data-zone="left"
            data-active={activePreview === "left" ? "true" : "false"}
            data-testid="edge-dock-snap-left"
            aria-label={t("settings.advanced.edgeDock.positions.left")}
          />
          <div
            className="ed-snap-zone"
            data-zone="bottom"
            data-active={activePreview === "bottom" ? "true" : "false"}
            data-testid="edge-dock-snap-bottom"
            aria-label={t("settings.advanced.edgeDock.positions.bottom")}
          />
          <div
            className="ed-snap-zone"
            data-zone="right"
            data-active={activePreview === "right" ? "true" : "false"}
            data-testid="edge-dock-snap-right"
            aria-label={t("settings.advanced.edgeDock.positions.right")}
          />
          <div
            className="ed-drag-puck"
            style={{ left: `${puckX}px`, top: `${puckY}px` }}
            aria-hidden="true"
          >
            <DictateMicGlyph />
          </div>
        </div>
      </div>
    );
  }

  return (
    <div
      className="ed-stage"
      data-testid="edge-dock-stage"
      data-mode={state.mode}
      data-edge={state.position}
      data-menu-open={state.menu_open ? "true" : "false"}
    >
      {/* 1. Tactile Whisper-Bar (46×9px capsule inside flush 76×24px zero-dead-zone hit button) */}
      <button
        type="button"
        className="ed-sliver"
        data-edge={state.position}
        data-visible={!isExpanded ? "true" : "false"}
        data-meeting-active={meetingActive ? "true" : "false"}
        data-testid="edge-dock-sliver"
        aria-label={t("edgeDock.openQuickActions")}
        tabIndex={isExpanded ? -1 : 0}
        onPointerDown={handlePointerDownForDrag}
        onMouseEnter={(e) => {
          if (leaveTimerRef.current !== null) {
            window.clearTimeout(leaveTimerRef.current);
            leaveTimerRef.current = null;
          }
          if (
            e.buttons !== 0 ||
            suppressHoverUntilLeaveRef.current ||
            dragStartedRef.current
          ) {
            return;
          }
          setExpanded(true);
        }}
        onMouseLeave={() => {
          suppressHoverUntilLeaveRef.current = false;
        }}
        onClick={() => {
          suppressHoverUntilLeaveRef.current = false;
          if (!dragStartedRef.current) {
            setExpanded(true);
          }
        }}
        onContextMenu={(e) => {
          e.preventDefault();
          suppressHoverUntilLeaveRef.current = false;
          setHoveredAction(null);
          setMenuKind("context");
          setMenuOpen(true);
        }}
      >
        <span className="ed-hairline" aria-hidden="true" />
      </button>

      {/* 2. Persistent-DOM 3 Detached macOS Dark-Glass Pills (zero inline text, hover-only tooltip) */}
      <div
        ref={clusterRef}
        className="ed-expanded-wrap"
        data-edge={state.position}
        data-visible={isExpanded ? "true" : "false"}
        data-testid="edge-dock-expanded"
        onPointerDown={handlePointerDownForDrag}
        onPointerMove={(e) => {
          if (dragStartedRef.current || state.menu_open) return;
          const next = resolveHoveredActionAtPoint(e.clientX, e.clientY);
          setHoveredAction((curr) => (curr === next ? curr : next));
        }}
        onMouseEnter={() => {
          if (leaveTimerRef.current !== null) {
            window.clearTimeout(leaveTimerRef.current);
            leaveTimerRef.current = null;
          }
        }}
        onMouseLeave={() => {
          setHoveredAction(null);
          if (dragStartedRef.current || state.menu_open) return;
          if (leaveTimerRef.current !== null) {
            window.clearTimeout(leaveTimerRef.current);
          }
          leaveTimerRef.current = window.setTimeout(() => {
            leaveTimerRef.current = null;
            if (!dragStartedRef.current && !state.menu_open) {
              setExpanded(false);
            }
          }, LEAVE_COLLAPSE_MS);
        }}
        onContextMenu={(e) => {
          e.preventDefault();
          setHoveredAction(null);
          setMenuKind("context");
          setMenuOpen(true);
        }}
      >
        <div
          ref={clusterRowRef}
          className="ed-cluster"
          onPointerDown={handlePointerDownForDrag}
          onMouseLeave={() => setHoveredAction(null)}
        >
          <button
            ref={dictateBtnRef}
            type="button"
            className="ed-pill ed-pill-dictate"
            data-action="dictate"
            data-hovered={hoveredAction === "dictate" ? "true" : "false"}
            data-testid="edge-dock-action-dictate"
            aria-label={t("edgeDock.actions.dictate")}
            tabIndex={isExpanded ? 0 : -1}
            onMouseEnter={() => setHoveredAction("dictate")}
            onFocus={() => setHoveredAction("dictate")}
            onBlur={() =>
              setHoveredAction((curr) => (curr === "dictate" ? null : curr))
            }
            onClick={() => {
              if (dragStartedRef.current) return;
              setMenuOpen(false);
              if (hasTauri) {
                void commands.triggerDockDictation();
              } else {
                setExpanded(false);
              }
            }}
          >
            <DictateMicGlyph />
          </button>

          <button
            ref={meetingBtnRef}
            type="button"
            className="ed-pill ed-pill-circle"
            data-action="meeting"
            data-meeting-active={meetingActive ? "true" : "false"}
            data-hovered={hoveredAction === "meeting" ? "true" : "false"}
            data-testid="edge-dock-action-meeting"
            aria-label={t("edgeDock.actions.meeting")}
            tabIndex={isExpanded ? 0 : -1}
            onMouseEnter={() => setHoveredAction("meeting")}
            onFocus={() => setHoveredAction("meeting")}
            onBlur={() =>
              setHoveredAction((curr) => (curr === "meeting" ? null : curr))
            }
            onClick={() => {
              if (dragStartedRef.current) return;
              setMenuOpen(false);
              if (hasTauri) {
                void commands.triggerDockMeeting();
              } else {
                setExpanded(false);
              }
            }}
          >
            <MeetingRecordGlyph />
            {meetingActive ? (
              <span className="ed-live-meeting-dot" aria-hidden="true" />
            ) : null}
          </button>

          <button
            ref={transformBtnRef}
            type="button"
            className="ed-pill ed-pill-transform"
            data-action="transform"
            data-hovered={hoveredAction === "transform" ? "true" : "false"}
            data-open={
              state.menu_open && menuKind === "transform" ? "true" : "false"
            }
            data-armed={state.auto_apply_enabled ? "true" : "false"}
            data-testid="edge-dock-action-transform"
            aria-label={t("edgeDock.actions.transform")}
            aria-expanded={state.menu_open && menuKind === "transform"}
            tabIndex={isExpanded ? 0 : -1}
            onMouseEnter={() => setHoveredAction("transform")}
            onFocus={() => setHoveredAction("transform")}
            onBlur={() =>
              setHoveredAction((curr) => (curr === "transform" ? null : curr))
            }
            onClick={() => {
              if (dragStartedRef.current) return;
              if (state.menu_open && menuKind === "transform") {
                setMenuOpen(false);
              } else {
                setMenuKind("transform");
                setMenuOpen(true);
              }
            }}
          >
            <TransformWandGlyph />
            <svg
              className="ed-chevron"
              width="7"
              height="7"
              viewBox="0 0 8 8"
              fill="none"
              aria-hidden="true"
            >
              <path
                d="M1.6 2.8L4 5.2L6.4 2.8"
                stroke="currentColor"
                strokeWidth="1.35"
                strokeLinecap="round"
                strokeLinejoin="round"
              />
            </svg>
            {state.auto_apply_enabled ? (
              <span className="ed-armed-dot" aria-hidden="true" />
            ) : null}
          </button>
        </div>

        {!state.menu_open && hoveredAction ? (
          <div
            ref={readoutRef}
            className="ed-readout"
            data-edge={state.position}
            data-gliding={tipGliding ? "true" : "false"}
            data-action={hoveredAction}
            data-testid="edge-dock-readout"
            style={
              {
                "--ed-tip-offset": `${tipOffsetPx}px`,
              } as React.CSSProperties
            }
          >
            <span className="ed-readout-label">{readoutLabel}</span>
            {readoutKbd ? (
              <kbd className="ed-readout-kbd">{readoutKbd}</kbd>
            ) : null}
          </div>
        ) : null}
      </div>

      {/* 3. Native macOS HUD Popover (Transform Presets on left-click, Snooze/Hide on right-click) */}
      {state.menu_open ? (
        <div
          ref={popoverRef}
          className="ed-popover"
          data-edge={state.position}
          data-kind={menuKind}
          data-testid="edge-dock-transform-menu"
          role="dialog"
          aria-label={t("overlay.transformMenuAria", {
            defaultValue: "Transform presets",
          })}
        >
          {menuKind === "context" ? (
            <div className="ed-preset-list">
              <button
                type="button"
                className="ed-preset-item"
                data-testid="edge-dock-snooze-btn"
                onClick={handleSnoozeOneHour}
              >
                <span className="ed-preset-title">
                  {t("edgeDock.snoozeOneHour", {
                    defaultValue: "Snooze for 1 hour",
                  })}
                </span>
              </button>
              <button
                type="button"
                className="ed-preset-item ed-danger-item"
                data-testid="edge-dock-hide-btn"
                onClick={handleHideDock}
              >
                <span className="ed-preset-title">
                  {t("edgeDock.hide", {
                    defaultValue: "Hide Edge Dock",
                  })}
                </span>
              </button>
            </div>
          ) : (
            <>
              <button
                type="button"
                className="ed-run-row"
                data-testid="edge-dock-run-transform"
                onClick={() => void handleRunTransformNow(null)}
              >
                <span className="ed-run-label">
                  <strong>{activePresetDisplay}</strong>
                </span>
                {state.transform_shortcut ? (
                  <kbd className="ed-readout-kbd">
                    {state.transform_shortcut}
                  </kbd>
                ) : null}
              </button>

              <div className="ed-preset-list" role="listbox">
                {state.presets.map((preset) => {
                  const selected = preset.id === activePreset.id;
                  return (
                    <button
                      key={preset.id}
                      type="button"
                      role="option"
                      aria-selected={selected}
                      className="ed-preset-item"
                      data-selected={selected ? "true" : "false"}
                      data-testid={`edge-dock-preset-${preset.id}`}
                      onClick={() => void handleSelectPreset(preset)}
                    >
                      <span className="ed-preset-title">
                        {formatPresetTitle(preset)}
                      </span>
                      {selected ? (
                        <span className="ed-check" aria-hidden="true">
                          ✓
                        </span>
                      ) : null}
                    </button>
                  );
                })}
              </div>

              <div className="ed-divider" />

              <button
                type="button"
                className="ed-auto-row"
                role="switch"
                aria-checked={state.auto_apply_enabled}
                data-testid="edge-dock-auto-toggle"
                onClick={() => void handleToggleAutoApply()}
              >
                <span>
                  {t("overlay.transformAutoApply", {
                    defaultValue: "Auto-polish",
                  })}
                </span>
                <span
                  className="ed-switch"
                  data-on={state.auto_apply_enabled ? "true" : "false"}
                  aria-hidden="true"
                >
                  <span className="ed-switch-thumb" />
                </span>
              </button>

              <button
                type="button"
                className="ed-foot-btn"
                data-testid="edge-dock-configure-btn"
                onClick={() => void handleOpenConfigure()}
              >
                <span>
                  {t("overlay.transformConfigure", {
                    defaultValue: "Configure…",
                  })}
                </span>
              </button>
            </>
          )}
        </div>
      ) : null}
    </div>
  );
};

export default EdgeDock;
