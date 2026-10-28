import React, { useId } from "react";
import { useTranslation } from "react-i18next";
import type { Tone } from "@/bindings";
import { TONES } from "./toneRules";

interface TonePickerProps {
  value: Tone;
  onChange: (tone: Tone) => void;
  /** Accessible name of the group, e.g. "Tone for Slack". */
  label: string;
  disabled?: boolean;
}

/**
 * Segmented control built from native radio inputs, so arrow keys,
 * Tab and screen readers behave as a standard radio group.
 */
export const TonePicker: React.FC<TonePickerProps> = ({
  value,
  onChange,
  label,
  disabled = false,
}) => {
  const { t } = useTranslation();
  const name = useId();

  return (
    <div
      role="radiogroup"
      aria-label={label}
      aria-disabled={disabled || undefined}
      className="inline-flex shrink-0 rounded-md border border-line-strong bg-wash p-0.5"
    >
      {TONES.map((tone) => {
        const checked = tone === value;
        return (
          <label
            key={tone}
            className={`relative cursor-pointer rounded px-2.5 py-1 text-xs font-medium transition-colors has-[:focus-visible]:ring-1 has-[:focus-visible]:ring-logo-primary ${
              checked
                ? "bg-background-ui text-white"
                : "text-ink-2 hover:bg-logo-primary/15 hover:text-text"
            } ${disabled ? "cursor-not-allowed opacity-50" : ""}`}
          >
            <input
              type="radio"
              name={name}
              value={tone}
              checked={checked}
              disabled={disabled}
              onChange={() => onChange(tone)}
              className="sr-only"
            />
            {t(`flow.tones.${tone}.label`)}
          </label>
        );
      })}
    </div>
  );
};
