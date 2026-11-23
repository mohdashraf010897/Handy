# lowyap privacy note

Effective 29 October 2026 · applies to lowyap 0.1.x (desktop app)

**Short version:** your voice never leaves your computer. lowyap has no
accounts and no ads, and lowyap itself includes no telemetry or analytics. It
only goes online for the things listed below.

## What stays on your device

| Data                                 | Where                                                                                                                    | How long                                                                                                                                                                             |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Microphone audio and transcripts     | Captured only while your shortcut is active, transcribed by a local model, kept in the local history                     | By default the last 5 unsaved dictations are kept so you can replay them. Dictations you save and files you transcribe stay until you delete them. Change this in Settings → History |
| Settings, history, downloaded models | App data folder (`%APPDATA%\com.lowyap.desktop` on Windows, `~/Library/Application Support/com.lowyap.desktop` on macOS) | Until you delete them. Uninstalling may not remove these files                                                                                                                       |
| Logs (`lowyap.log`)                  | Your system's app log folder                                                                                             | Until you delete them                                                                                                                                                                |
| Name of the app you are typing into  | Read locally to choose a writing style (per-app tone)                                                                    | Never stored with your text, never sent anywhere                                                                                                                                     |

When lowyap pastes through the clipboard, it briefly replaces the clipboard
contents. Previous text or images are put back where supported; other clipboard
formats may not be preserved.

On macOS, **macOS Instant Voice** needs no lowyap model download. It requires
macOS Speech Recognition permission and an available on-device speech language.
lowyap verifies on-device support and sets `requiresOnDeviceRecognition = true`
on every request. Audio is never sent to Apple or another server. If a supported
on-device language isn't available, transcription fails locally rather than
switching to cloud recognition.

## When lowyap connects to the internet

| Connection                                                                                                                             | When                                                              | What is sent                                                                                                                                                                                                                                                      |
| -------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Update check** to GitHub (`github.com/mohdashraf010897/lowyap`)                                                                      | At start-up, unless you turn off update checks in Settings        | A standard web request. GitHub sees your IP address ([GitHub privacy statement](https://docs.github.com/site-policy/privacy-policies/github-general-privacy-statement))                                                                                           |
| **Model download** from Hugging Face (`huggingface.co`) or the lowyap model mirror (`dl.lowyap.com`) if configured | Only when you choose to download a speech model or language detector | A standard file download. The host sees your IP address, user agent and requested file. No audio or transcript text is sent ([Hugging Face privacy policy](https://huggingface.co/privacy)) |
| **Optional AI clean-up** with a provider you choose (for example OpenAI, Anthropic, Groq, OpenRouter, or a local server)               | Off by default. Only after you enable it and add your own API key | Your transcript text, clean-up prompt, API key and standard request metadata go to that provider under **your** account and **their** privacy policy. lowyap identifies itself in request headers. Audio and the name of the app you are typing into are not sent |

Links you click (for example "Help" or the GitHub page) open in your browser.

## Your API keys

If you add an API key for AI clean-up, it is stored **unencrypted** in
`settings_store.json` in the app data folder. Anyone with access to your user
account can read it. Use a key with a spending limit and revoke it if your
computer is shared or lost. Moving keys to the operating system's credential
store is planned.

## Children

lowyap is intended for people aged 18 and over.

## Changes and contact

Changes to this note are listed in the release notes.

- Privacy questions: email <privacy@lowyap.com>. Do not send recordings or API keys.
- Security vulnerabilities: report privately through the repository's
  **Security → Report a vulnerability** page.
