import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { DoneMark } from "../shared/DoneMark";
import { prefersReducedMotion } from "@/lib/utils/motion";
import {
  resolveWalkthroughActiveStep,
  STEP_2_START_SEC,
  type WalkthroughStep,
  type WalkthroughStepResolution,
} from "./macZeroWaitHelpers";
import type { Permissions as PermissionsState } from "./usePermissions";

export { resolveWalkthroughActiveStep, type WalkthroughStep, type WalkthroughStepResolution };

interface OnboardingWalkthroughProps {
  microphone: PermissionsState["microphone"];
  accessibility: PermissionsState["accessibility"];
  allGranted: boolean;
}

/**
 * Cairn-style embedded macOS permissions walkthrough card:
 * - Two step pills ("1. Allow Microphone" -> "2. Flip lowyap ON in Accessibility")
 *   automatically synced to whichever permission is currently active.
 * - Deterministic 8-second real-macOS-styled looping micro-video (.webm/.mp4)
 *   with reduced-motion static fallback.
 * - Live polling auto-advance visual cue so users know System Settings changes
 *   update this screen automatically.
 */
export const OnboardingWalkthrough: React.FC<OnboardingWalkthroughProps> = ({
  microphone,
  accessibility,
  allGranted,
}) => {
  const { t } = useTranslation();
  const videoRef = useRef<HTMLVideoElement>(null);
  const [reducedMotion] = useState(prefersReducedMotion);

  const { activePermissionStep, micGranted, accGranted, seekTimeSec } =
    resolveWalkthroughActiveStep(microphone, accessibility);

  const [manualStep, setManualStep] = useState<WalkthroughStep | null>(null);
  const currentStep = manualStep ?? activePermissionStep;

  // Automatically sync step selection and video seek position when permission state changes
  useEffect(() => {
    setManualStep(null);
    const video = videoRef.current;
    if (!video || reducedMotion) return;
    try {
      video.currentTime = seekTimeSec;
      void video.play().catch(() => {});
    } catch {
      // Ignore seek errors before metadata is loaded
    }
  }, [micGranted, accGranted, reducedMotion, seekTimeSec]);

  const jumpToStep = (step: WalkthroughStep) => {
    setManualStep(step);
    const video = videoRef.current;
    if (!video || reducedMotion) return;
    try {
      video.currentTime = step === "accessibility" ? STEP_2_START_SEC : 0;
      void video.play().catch(() => {});
    } catch {
      // Ignore seek errors before metadata is loaded
    }
  };

  return (
    <section
      data-testid="onboarding-macos-walkthrough"
      aria-label={t("onboarding.permissions.walkthrough.ariaLabel", {
        defaultValue: "macOS permissions visual walkthrough",
      })}
      className="mt-[10px] flex w-full flex-col overflow-hidden rounded-panel border border-line-strong bg-surface text-start"
    >
      {/* Synced 2-Step Pills Header */}
      <div className="flex flex-wrap items-center justify-between gap-[8px] border-b border-line bg-background px-[14px] py-[8px]">
        <div
          role="group"
          aria-label={t("onboarding.permissions.walkthrough.stepsGroupLabel", {
            defaultValue: "Permission setup steps",
          })}
          className="flex flex-wrap items-center gap-[6px]"
        >
          <button
            type="button"
            data-testid="walkthrough-pill-microphone"
            aria-pressed={currentStep === "microphone"}
            onClick={() => jumpToStep("microphone")}
            className={`inline-flex cursor-pointer items-center gap-[6px] rounded-full border px-[10px] py-[3px] text-label font-semibold transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary ${
              micGranted
                ? "border-moss/30 bg-moss-wash text-moss"
                : currentStep === "microphone"
                  ? "border-logo-primary bg-surface text-text"
                  : "border-line bg-wash text-ink-2 hover:text-text"
            }`}
          >
            {micGranted ? (
              <DoneMark />
            ) : (
              <span
                aria-hidden="true"
                className="inline-block size-[6px] rounded-full bg-logo-primary"
              />
            )}
            <span>
              {t("onboarding.permissions.walkthrough.step1Pill", {
                defaultValue: "1. Allow Microphone",
              })}
            </span>
          </button>

          <span
            aria-hidden="true"
            className="font-mono text-label text-ink-3 select-none"
          >
            {"\u2192"}
          </span>

          <button
            type="button"
            data-testid="walkthrough-pill-accessibility"
            aria-pressed={currentStep === "accessibility"}
            onClick={() => jumpToStep("accessibility")}
            className={`inline-flex cursor-pointer items-center gap-[6px] rounded-full border px-[10px] py-[3px] text-label font-semibold transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary ${
              accGranted
                ? "border-moss/30 bg-moss-wash text-moss"
                : currentStep === "accessibility"
                  ? "border-logo-primary bg-surface text-text"
                  : "border-line bg-wash text-ink-2 hover:text-text"
            }`}
          >
            {accGranted ? (
              <DoneMark />
            ) : (
              <span
                aria-hidden="true"
                className={`inline-block size-[6px] rounded-full ${
                  currentStep === "accessibility"
                    ? "bg-logo-primary"
                    : "bg-ink-3"
                }`}
              />
            )}
            <span>
              {t("onboarding.permissions.walkthrough.step2Pill", {
                defaultValue: "2. Flip lowyap ON in Accessibility",
              })}
            </span>
          </button>
        </div>

        <span className="font-mono text-tag font-medium text-ink-3">
          {currentStep === "microphone"
            ? t("onboarding.permissions.walkthrough.step1Badge", {
                defaultValue: "macOS Prompt",
              })
            : t("onboarding.permissions.walkthrough.step2Badge", {
                defaultValue: "Privacy & Security \u203a Accessibility",
              })}
        </span>
      </div>

      {/* Deterministic 8s Real-Mac Looping Video or Reduced-Motion Static Fallback */}
      <div className="relative w-full bg-background">
        {reducedMotion ? (
          <div
            data-testid="walkthrough-reduced-motion-fallback"
            className="grid grid-cols-[1fr_auto] items-center gap-[14px] px-[16px] py-[12px]"
          >
            <div className="flex flex-col gap-[4px]">
              <b className="font-display text-small font-[650] text-text">
                {currentStep === "microphone"
                  ? t("onboarding.permissions.walkthrough.staticStep1Title", {
                      defaultValue:
                        "Step 1: Click “Allow” when macOS asks for Microphone access",
                    })
                  : t("onboarding.permissions.walkthrough.staticStep2Title", {
                      defaultValue:
                        "Step 2: In System Settings \u2192 Privacy & Security \u2192 Accessibility, flip the lowyap switch ON",
                    })}
              </b>
              <p className="m-0 text-label text-ink-2">
                {t("onboarding.permissions.walkthrough.staticSub", {
                  defaultValue:
                    "Once both switches are enabled, the green Allowed checkmarks light up automatically.",
                })}
              </p>
            </div>
            <img
              src="/onboarding/macos-permissions-guide-poster.png"
              alt={t("onboarding.permissions.walkthrough.posterAlt", {
                defaultValue:
                  "macOS System Settings showing lowyap enabled in Privacy and Security Accessibility",
              })}
              className="h-[72px] w-[196px] rounded-card border border-line object-cover"
            />
          </div>
        ) : (
          <video
            ref={videoRef}
            data-testid="walkthrough-video"
            muted
            autoPlay
            loop
            playsInline
            poster="/onboarding/macos-permissions-guide-poster.png"
            onLoadedMetadata={(e) => {
              if (micGranted && !accGranted) {
                e.currentTarget.currentTime = STEP_2_START_SEC;
              }
            }}
            aria-label={t("onboarding.permissions.walkthrough.videoLabel", {
              defaultValue:
                "8-second visual walkthrough showing how to click Allow on the macOS Microphone prompt and flip the lowyap switch ON in System Settings under Privacy and Security, Accessibility.",
            })}
            className="block h-[132px] w-full object-cover"
          >
            <source
              src="/onboarding/macos-permissions-guide.webm"
              type="video/webm"
            />
            <source
              src="/onboarding/macos-permissions-guide.mp4"
              type="video/mp4"
            />
          </video>
        )}
      </div>

      {/* Live Polling Auto-Advance Cue */}
      <div className="flex items-center justify-between gap-[10px] border-t border-line bg-background px-[14px] py-[6px] text-label">
        <span className="inline-flex items-center gap-[7px] font-medium text-ink-2">
          <span
            aria-hidden="true"
            className={`inline-block size-[7px] shrink-0 rounded-full ${
              allGranted ? "bg-moss" : "animate-pulse bg-logo-primary"
            }`}
          />
          <span>
            {allGranted
              ? t("onboarding.permissions.walkthrough.allGrantedCue", {
                  defaultValue:
                    "Both permissions granted \u2713 \u2014 click Continue below",
                })
              : t("onboarding.permissions.walkthrough.autoAdvanceHint", {
                  defaultValue:
                    "Flip the switch in System Settings \u2014 this screen updates automatically",
                })}
          </span>
        </span>
        <span className="shrink-0 font-mono text-tag text-ink-3">
          {t("onboarding.permissions.walkthrough.livePollingBadge", {
            defaultValue: "Live auto-detect",
          })}
        </span>
      </div>
    </section>
  );
};

export default OnboardingWalkthrough;
