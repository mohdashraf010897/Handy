import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { isLoopbackBaseUrl, resolveStudioEgressState } from "./egressState";

describe("resolveStudioEgressState", () => {
  it("reports zero_egress when local ASR is paired with disabled post-processing", () => {
    const state = resolveStudioEgressState(
      { is_cloud: false },
      {
        post_process_enabled: false,
        post_process_provider_id: "openai",
        post_process_providers: [],
      },
    );
    assert.equal(state.mode, "zero_egress");
    assert.equal(state.hasCloudEgress, false);
  });

  it("reports zero_egress for Apple Intelligence and loopback Ollama/LM Studio endpoints", () => {
    const apple = resolveStudioEgressState(
      { is_cloud: false },
      {
        post_process_enabled: true,
        post_process_provider_id: "apple_intelligence",
        post_process_providers: [],
      },
    );
    assert.equal(apple.mode, "zero_egress");

    const ollama = resolveStudioEgressState(
      { is_cloud: false },
      {
        post_process_enabled: true,
        post_process_provider_id: "custom",
        post_process_providers: [
          {
            id: "custom",
            label: "Ollama Local",
            base_url: "http://localhost:11434/v1",
            allow_base_url_edit: true,
            models_endpoint: "/models",
            supports_structured_output: false,
            api_format: "openai",
          },
        ],
      },
    );
    assert.equal(ollama.mode, "zero_egress");
    assert.equal(isLoopbackBaseUrl("http://127.0.0.1:1234/v1"), true);
    assert.equal(isLoopbackBaseUrl("http://[::1]:8080/v1"), true);
  });

  it("reports cloud_llm when post-processing sends transcripts to a remote provider", () => {
    const state = resolveStudioEgressState(
      { is_cloud: false },
      {
        post_process_enabled: true,
        post_process_provider_id: "openai",
        post_process_providers: [
          {
            id: "openai",
            label: "OpenAI",
            base_url: "https://api.openai.com/v1",
            allow_base_url_edit: false,
            models_endpoint: "/models",
            supports_structured_output: true,
            api_format: "openai",
          },
        ],
      },
    );
    assert.equal(state.mode, "cloud_llm");
    assert.equal(state.hasCloudEgress, true);
    assert.equal(state.cloudLinguist, true);
    assert.equal(state.cloudAsr, false);
  });

  it("fails closed to cloud_llm when post-processing is enabled with an unverified provider id", () => {
    const state = resolveStudioEgressState(
      { is_cloud: false },
      {
        post_process_enabled: true,
        post_process_provider_id: "anthropic",
        post_process_providers: [],
      },
    );
    assert.equal(state.mode, "cloud_llm");
    assert.equal(state.hasCloudEgress, true);
  });

  it("reports cloud_asr and cloud_both accurately", () => {
    const asrOnly = resolveStudioEgressState(
      { is_cloud: true },
      {
        post_process_enabled: false,
        post_process_provider_id: "openai",
        post_process_providers: [],
      },
    );
    assert.equal(asrOnly.mode, "cloud_asr");
    assert.equal(asrOnly.hasCloudEgress, true);

    const both = resolveStudioEgressState(
      { is_cloud: true },
      {
        post_process_enabled: true,
        post_process_provider_id: "openai",
        post_process_providers: [],
      },
    );
    assert.equal(both.mode, "cloud_both");
    assert.equal(both.hasCloudEgress, true);
  });
});
