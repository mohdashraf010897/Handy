import React, { useCallback, useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { RefreshCw, RotateCcw, Trash2 } from "lucide-react";
import type { AppToneRule, FlowSettings, LastFlowApp, Tone } from "@/bindings";
import { useSettings } from "@/hooks/useSettings";
import { useFlowStore } from "@/stores/flowStore";
import { Alert } from "../ui/Alert";
import { Button } from "../ui/Button";
import { Dialog } from "../ui/Dialog";
import { Input } from "../ui/Input";
import { SettingsGroup } from "../ui/SettingsGroup";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { PostProcessingToggle } from "../settings/PostProcessingToggle";
import { TonePicker } from "./TonePicker";
import {
  type AppIdError,
  TONES,
  findRuleIndex,
  overridingRule,
  validateAppId,
} from "./toneRules";

const DEFAULT_FLOW_SETTINGS: FlowSettings = {
  tone_enabled: true,
  app_tones: [],
};

const ruleInputId = (index: number) => `flow-rule-app-${index}`;

const Row: React.FC<{ children: React.ReactNode; className?: string }> = ({
  children,
  className = "",
}) => <div className={`px-4 py-3 ${className}`}>{children}</div>;

const FieldError: React.FC<{ id: string; error: AppIdError | null }> = ({
  id,
  error,
}) => {
  const { t } = useTranslation();
  if (!error) return null;
  return (
    <p id={id} role="alert" className="mt-1 text-xs text-error">
      {t(`flow.rules.errors.${error}`)}
    </p>
  );
};

interface RuleRowProps {
  rule: AppToneRule;
  index: number;
  rules: AppToneRule[];
  disabled: boolean;
  onChange: (index: number, rule: AppToneRule) => void;
  onRemove: (index: number) => void;
}

const RuleRow: React.FC<RuleRowProps> = ({
  rule,
  index,
  rules,
  disabled,
  onChange,
  onRemove,
}) => {
  const { t } = useTranslation();
  const errorId = useId();
  const [draft, setDraft] = useState(rule.app_id);
  const [error, setError] = useState<AppIdError | null>(null);

  useEffect(() => {
    setDraft(rule.app_id);
    setError(null);
  }, [rule.app_id]);

  const commit = () => {
    if (draft.trim() === rule.app_id) {
      setError(null);
      return;
    }
    const nextError = validateAppId(draft, rules, index);
    setError(nextError);
    if (!nextError) onChange(index, { ...rule, app_id: draft.trim() });
  };

  const appLabel = draft.trim() || rule.app_id;

  return (
    <Row>
      <div className="flex flex-wrap items-center gap-2">
        <div className="min-w-[12rem] flex-1">
          <Input
            id={ruleInputId(index)}
            variant="compact"
            className="w-full font-mono"
            value={draft}
            disabled={disabled}
            spellCheck={false}
            autoCapitalize="off"
            autoCorrect="off"
            aria-label={t("flow.rules.appIdFor", { index: index + 1 })}
            aria-invalid={error ? true : undefined}
            aria-describedby={error ? errorId : undefined}
            onChange={(event) => setDraft(event.target.value)}
            onBlur={commit}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                commit();
              } else if (event.key === "Escape") {
                setDraft(rule.app_id);
                setError(null);
              }
            }}
          />
        </div>
        <TonePicker
          value={rule.tone}
          disabled={disabled}
          label={t("flow.rules.toneFor", { app: appLabel })}
          onChange={(tone) => onChange(index, { ...rule, tone })}
        />
        <Button
          variant="danger-ghost"
          size="sm"
          disabled={disabled}
          onClick={() => onRemove(index)}
          aria-label={t("flow.rules.remove", { app: rule.app_id })}
          title={t("flow.rules.remove", { app: rule.app_id })}
          className="flex items-center"
        >
          <Trash2 className="h-4 w-4" aria-hidden="true" />
        </Button>
      </div>
      <FieldError id={errorId} error={error} />
    </Row>
  );
};

const AddRuleForm: React.FC<{
  rules: AppToneRule[];
  disabled: boolean;
  onAdd: (rule: AppToneRule) => Promise<boolean>;
}> = ({ rules, disabled, onAdd }) => {
  const { t } = useTranslation();
  const inputId = useId();
  const errorId = useId();
  const [appId, setAppId] = useState("");
  const [tone, setTone] = useState<Tone>("casual");
  const [error, setError] = useState<AppIdError | null>(null);

  const submit = async () => {
    const nextError = validateAppId(appId, rules);
    setError(nextError);
    if (nextError) return;
    if (await onAdd({ app_id: appId.trim(), tone })) setAppId("");
  };

  return (
    <Row>
      <label
        htmlFor={inputId}
        className="mb-1 block text-sm font-medium text-text"
      >
        {t("flow.rules.add.label")}
      </label>
      <div className="flex flex-wrap items-center gap-2">
        <Input
          id={inputId}
          variant="compact"
          className="min-w-[12rem] flex-1 font-mono"
          value={appId}
          disabled={disabled}
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          placeholder={t("flow.rules.add.placeholder")}
          aria-invalid={error ? true : undefined}
          aria-describedby={error ? errorId : undefined}
          onChange={(event) => {
            setAppId(event.target.value);
            if (error) setError(null);
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              void submit();
            }
          }}
        />
        <TonePicker
          value={tone}
          onChange={setTone}
          disabled={disabled}
          label={t("flow.rules.add.toneLabel")}
        />
        <Button
          variant="primary"
          size="sm"
          disabled={disabled}
          onClick={() => void submit()}
        >
          {t("flow.rules.add.button")}
        </Button>
      </div>
      <FieldError id={errorId} error={error} />
      <p className="mt-1 text-xs text-ink-3">{t("flow.rules.add.hint")}</p>
    </Row>
  );
};

const LastAppCard: React.FC<{
  lastApp: LastFlowApp | null;
  rules: AppToneRule[];
  disabled: boolean;
  onSetTone: (appId: string, tone: Tone) => void;
  onRefresh: () => void;
}> = ({ lastApp, rules, disabled, onSetTone, onRefresh }) => {
  const { t } = useTranslation();
  const refreshButton = (
    <Button
      variant="ghost"
      size="sm"
      onClick={onRefresh}
      className="flex shrink-0 items-center gap-1"
    >
      <RefreshCw className="h-3.5 w-3.5" aria-hidden="true" />
      {t("flow.lastApp.refresh")}
    </Button>
  );

  if (!lastApp) {
    return (
      <Row className="flex items-center justify-between gap-3">
        <p className="text-sm text-ink-3">{t("flow.lastApp.empty")}</p>
        {refreshButton}
      </Row>
    );
  }

  const hasOwnRule = findRuleIndex(rules, lastApp.app_id) !== -1;
  const name = lastApp.app_name || lastApp.app_id;
  const toneLabel = t(`flow.tones.${lastApp.tone}.label`);
  const status = !lastApp.matched_rule
    ? t("flow.lastApp.noRule", { tone: toneLabel })
    : lastApp.user_rule
      ? t("flow.lastApp.userRule", {
          tone: toneLabel,
          rule: lastApp.matched_rule,
        })
      : t("flow.lastApp.builtinRule", {
          tone: toneLabel,
          rule: lastApp.matched_rule,
        });

  return (
    <Row>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="min-w-0">
          <p className="truncate text-sm font-medium text-text">{name}</p>
          <p className="truncate font-mono text-xs text-ink-3">
            {lastApp.app_id}
          </p>
          <p className="mt-1 text-xs text-ink-2">{status}</p>
        </div>
        {refreshButton}
      </div>
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <span className="text-xs text-ink-2" aria-hidden="true">
          {hasOwnRule ? t("flow.lastApp.change") : t("flow.lastApp.add")}
        </span>
        <TonePicker
          value={lastApp.tone}
          disabled={disabled}
          label={t("flow.lastApp.pickerLabel", { app: name })}
          onChange={(tone) => onSetTone(lastApp.app_id, tone)}
        />
      </div>
    </Row>
  );
};

const BuiltinList: React.FC<{
  builtins: AppToneRule[];
  rules: AppToneRule[];
  disabled: boolean;
  onOverride: (rule: AppToneRule) => void;
}> = ({ builtins, rules, disabled, onOverride }) => {
  const { t } = useTranslation();

  return (
    <details className="group">
      <summary className="flex cursor-pointer list-none items-center justify-between px-4 py-3 text-sm font-medium text-text hover:bg-wash focus:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-logo-primary [&::-webkit-details-marker]:hidden">
        <span>{t("flow.builtins.summary", { count: builtins.length })}</span>
        <span
          className="text-ink-3 transition-transform group-open:rotate-90"
          aria-hidden="true"
        >
          ›
        </span>
      </summary>
      <div className="divide-y divide-line border-t border-line">
        {TONES.filter((tone) => tone !== "default").map((tone) => {
          const items = builtins.filter((rule) => rule.tone === tone);
          if (items.length === 0) return null;
          return (
            <div key={tone} className="px-4 py-3">
              <h3 className="mb-2 text-xs font-medium uppercase tracking-wide text-ink-3">
                {t(`flow.tones.${tone}.label`)}
              </h3>
              <ul className="space-y-1">
                {items.map((builtin) => {
                  const override = overridingRule(builtin, rules);
                  return (
                    <li
                      key={builtin.app_id}
                      className="flex items-center justify-between gap-2"
                    >
                      <span
                        className={`truncate font-mono text-xs ${
                          override ? "text-ink-3 line-through" : "text-ink-2"
                        }`}
                      >
                        {builtin.app_id}
                      </span>
                      {override ? (
                        <span className="shrink-0 text-xs text-ink-3">
                          {t("flow.builtins.overridden", {
                            tone: t(`flow.tones.${override.tone}.label`),
                          })}
                        </span>
                      ) : (
                        <Button
                          variant="ghost"
                          size="sm"
                          disabled={disabled}
                          className="shrink-0"
                          aria-label={t("flow.builtins.overrideFor", {
                            app: builtin.app_id,
                          })}
                          onClick={() => onOverride(builtin)}
                        >
                          {t("flow.builtins.override")}
                        </Button>
                      )}
                    </li>
                  );
                })}
              </ul>
            </div>
          );
        })}
      </div>
    </details>
  );
};

export const PerAppToneSettings: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting } = useSettings();
  const { settings, builtins, lastApp, isLoading, isSaving, load, save } =
    useFlowStore();
  const refreshLastApp = useFlowStore((state) => state.refreshLastApp);
  const [confirmReset, setConfirmReset] = useState(false);

  useEffect(() => {
    void load();
    const onFocus = () => void refreshLastApp();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [load, refreshLastApp]);

  const rules = settings?.app_tones ?? [];

  const saveRules = useCallback(
    (app_tones: AppToneRule[]) =>
      settings ? save({ ...settings, app_tones }) : Promise.resolve(false),
    [save, settings],
  );

  const focusRule = (index: number) =>
    requestAnimationFrame(() =>
      document.getElementById(ruleInputId(index))?.focus(),
    );

  const upsertTone = async (appId: string, tone: Tone) => {
    const index = findRuleIndex(rules, appId);
    if (index === -1) {
      await saveRules([...rules, { app_id: appId, tone }]);
    } else {
      await saveRules(
        rules.map((rule, i) => (i === index ? { ...rule, tone } : rule)),
      );
    }
  };

  const overrideBuiltin = async (builtin: AppToneRule) => {
    const index = findRuleIndex(rules, builtin.app_id);
    if (index !== -1) {
      focusRule(index);
      return;
    }
    if (await saveRules([...rules, { ...builtin }])) focusRule(rules.length);
  };

  if (isLoading && !settings) {
    return (
      <div className="max-w-3xl w-full mx-auto">
        <p className="px-4 text-sm text-ink-3" role="status">
          {t("flow.loading")}
        </p>
      </div>
    );
  }

  if (!settings) {
    return (
      <div className="max-w-3xl w-full mx-auto space-y-3">
        <Alert variant="error">{t("flow.loadError")}</Alert>
        <Button variant="secondary" size="sm" onClick={() => void load()}>
          {t("flow.retry")}
        </Button>
      </div>
    );
  }

  const postProcessEnabled = getSetting("post_process_enabled") ?? false;
  const isCustomized = !settings.tone_enabled || rules.length > 0;

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("flow.title")}>
        <ToggleSwitch
          checked={settings.tone_enabled}
          onChange={(tone_enabled) => void save({ ...settings, tone_enabled })}
          isUpdating={isSaving}
          label={t("flow.enable.label")}
          description={t("flow.enable.description")}
          descriptionMode="inline"
          grouped
        />
        <div className="space-y-2 px-4 py-3">
          {!postProcessEnabled && (
            <Alert variant="warning" contained className="rounded-md">
              {t("flow.notice.postProcessOff")}
            </Alert>
          )}
          <Alert variant="info" contained className="rounded-md">
            {postProcessEnabled
              ? t("flow.notice.postProcessOn")
              : t("flow.notice.postProcessHint")}{" "}
            {t("flow.notice.privacy")}
          </Alert>
        </div>
        {!postProcessEnabled && (
          <PostProcessingToggle descriptionMode="inline" grouped />
        )}
      </SettingsGroup>

      <SettingsGroup
        title={t("flow.lastApp.title")}
        description={t("flow.lastApp.description")}
      >
        <LastAppCard
          lastApp={lastApp}
          rules={rules}
          disabled={isSaving}
          onSetTone={(appId, tone) => void upsertTone(appId, tone)}
          onRefresh={() => void refreshLastApp()}
        />
      </SettingsGroup>

      <SettingsGroup
        title={t("flow.rules.title")}
        description={t("flow.rules.description")}
      >
        <AddRuleForm
          rules={rules}
          disabled={isSaving}
          onAdd={(rule) => saveRules([...rules, rule])}
        />
        {rules.length === 0 ? (
          <Row>
            <p className="text-sm text-ink-3">{t("flow.rules.empty")}</p>
          </Row>
        ) : (
          rules.map((rule, index) => (
            <RuleRow
              key={`${index}-${rule.app_id}`}
              rule={rule}
              index={index}
              rules={rules}
              disabled={isSaving}
              onChange={(i, next) =>
                void saveRules(rules.map((r, j) => (j === i ? next : r)))
              }
              onRemove={(i) => void saveRules(rules.filter((_, j) => j !== i))}
            />
          ))
        )}
        <Row className="flex items-center justify-between gap-3">
          <p className="text-xs text-ink-3">{t("flow.reset.hint")}</p>
          <Button
            variant="secondary"
            size="sm"
            disabled={!isCustomized || isSaving}
            onClick={() => setConfirmReset(true)}
            className="flex shrink-0 items-center gap-1"
          >
            <RotateCcw className="h-3.5 w-3.5" aria-hidden="true" />
            {t("flow.reset.button")}
          </Button>
        </Row>
      </SettingsGroup>

      <SettingsGroup
        title={t("flow.builtins.title")}
        description={t("flow.builtins.description")}
      >
        <BuiltinList
          builtins={builtins}
          rules={rules}
          disabled={isSaving}
          onOverride={(rule) => void overrideBuiltin(rule)}
        />
      </SettingsGroup>

      <SettingsGroup title={t("flow.tones.title")}>
        <dl className="divide-y divide-line">
          {TONES.map((tone) => (
            <div key={tone} className="px-4 py-3">
              <dt className="text-sm font-medium text-text">
                {t(`flow.tones.${tone}.label`)}
              </dt>
              <dd className="mt-0.5 text-xs text-ink-3">
                {t(`flow.tones.${tone}.description`)}
              </dd>
            </div>
          ))}
        </dl>
      </SettingsGroup>

      <Dialog
        open={confirmReset}
        onOpenChange={setConfirmReset}
        title={t("flow.reset.confirmTitle")}
        description={t("flow.reset.confirmBody")}
        closeLabel={t("flow.reset.cancel")}
        footer={
          <>
            <Button
              variant="secondary"
              size="sm"
              onClick={() => setConfirmReset(false)}
            >
              {t("flow.reset.cancel")}
            </Button>
            <Button
              variant="danger"
              size="sm"
              onClick={async () => {
                if (await save(DEFAULT_FLOW_SETTINGS)) setConfirmReset(false);
              }}
            >
              {t("flow.reset.confirm")}
            </Button>
          </>
        }
      >
        <p className="text-sm text-ink-2">{t("flow.reset.confirmDetail")}</p>
      </Dialog>
    </div>
  );
};
