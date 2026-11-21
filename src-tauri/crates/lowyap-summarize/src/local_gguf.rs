//! Local loopback GGUF (`Qwen3-4B-Instruct-2507`) OpenAI-compatible completion
//! backend and `D-19` local-only privacy firewall.

use std::time::Duration;

use lowyap_ports::PortError;
use serde::{Deserialize, Serialize};
use url::{Host, Url};

/// Canonical Hugging Face GGUF repository for local Qwen3-4B summarization.
pub const QWEN3_4B_GGUF_REPO: &str = "Qwen/Qwen3-4B-Instruct-2507-GGUF";

/// Default GGUF quantization balancing speed and multilingual fidelity.
pub const QWEN3_4B_DEFAULT_QUANT: &str = "Q4_K_M";

/// Equivalent Ollama model tag for users running Ollama locally.
pub const QWEN3_4B_OLLAMA_TAG: &str = "qwen3:4b-instruct";

/// Default maximum prompt characters for a 32K-context local Qwen3-4B server.
const DEFAULT_LOCAL_MAX_PROMPT_CHARS: usize = 24_000;

/// Launch specification for a local `llama-server` process serving a GGUF model
/// strictly on loopback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlamaServerLaunchSpec {
    /// Hugging Face repository containing the GGUF weights.
    pub hf_repo: String,
    /// Quantization suffix (for example `"Q4_K_M"`).
    pub quant: String,
    /// Bind address (defaults to `"127.0.0.1"`).
    pub host: String,
    /// Loopback listen port (defaults to `8080`).
    pub port: u16,
    /// Context window length in tokens (defaults to `32_768`).
    pub context_size: u32,
    /// Number of transformer layers offloaded to GPU/Metal (defaults to `99`).
    pub gpu_layers: u32,
}

impl Default for LlamaServerLaunchSpec {
    fn default() -> Self {
        Self {
            hf_repo: QWEN3_4B_GGUF_REPO.to_owned(),
            quant: QWEN3_4B_DEFAULT_QUANT.to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 8080,
            context_size: 32_768,
            gpu_layers: 99,
        }
    }
}

impl LlamaServerLaunchSpec {
    /// Builds CLI arguments suitable for spawning `llama-server`.
    pub fn build_cli_args(&self) -> Vec<String> {
        vec![
            "-hf".to_owned(),
            format!("{}:{}", self.hf_repo, self.quant),
            "--host".to_owned(),
            self.host.clone(),
            "--port".to_owned(),
            self.port.to_string(),
            "-c".to_owned(),
            self.context_size.to_string(),
            "-ngl".to_owned(),
            self.gpu_layers.to_string(),
        ]
    }

    /// Returns the OpenAI-compatible `/v1` base URL for this launch spec.
    pub fn endpoint_base_url(&self) -> String {
        format!("http://{}:{}/v1", self.host, self.port)
    }
}

/// Pluggable text-completion backend used by [`crate::HybridMapReduceSummarizer`]
/// and [`crate::generate_meeting_title`].
pub trait CompletionBackend: Send + Sync {
    /// Provenance identifier recorded on [`lowyap_ports::SummaryOutput::source_id`]
    /// (for example `"llm"`, `"local_gguf"`, or `"apple_intelligence"`).
    fn source_id(&self) -> &'static str;

    /// Maximum combined prompt size in characters accepted by this backend.
    fn max_prompt_chars(&self) -> usize;

    /// Returns `false` for backends (such as Apple Intelligence `FoundationModels`)
    /// that reject Perso-Arabic or Devanagari scripts.
    fn supports_non_latin_scripts(&self) -> bool {
        true
    }

    /// Executes a system + user prompt completion bounded by `max_tokens`.
    ///
    /// # Errors
    /// Returns a [`PortError`] when the backend is unavailable, blocked by
    /// policy, or returns an invalid response.
    fn complete(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        max_tokens: u32,
    ) -> Result<String, PortError>;
}

/// Validates a meeting LLM endpoint URL against the `D-19` local-only privacy
/// firewall.
///
/// Only `http` and `https` URLs without embedded credentials are accepted.
/// Unless `allow_cloud_egress` is explicitly `true`, the host MUST be an IPv4
/// loopback address (`127.0.0.0/8`), IPv6 loopback (`::1`), or the exact
/// ASCII case-insensitive domain `"localhost"`.
///
/// # Errors
/// Returns an `unavailable: ...` [`PortError`] when the URL is malformed,
/// embeds credentials, uses a disallowed scheme, or targets a non-loopback host
/// while `allow_cloud_egress` is `false`.
pub fn validate_meeting_endpoint_url(
    raw_url: &str,
    allow_cloud_egress: bool,
) -> Result<Url, PortError> {
    let trimmed = raw_url.trim();
    if trimmed.is_empty() {
        return Err("unavailable: meeting endpoint URL is empty".to_owned());
    }

    let parsed = Url::parse(trimmed)
        .map_err(|err| format!("unavailable: invalid meeting endpoint URL: {err}"))?;

    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!(
            "unavailable: unsupported meeting endpoint scheme '{}' (only http/https allowed)",
            parsed.scheme()
        ));
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(
            "unavailable: meeting endpoint URL must not contain embedded credentials".to_owned(),
        );
    }

    let host = parsed
        .host()
        .ok_or_else(|| "unavailable: meeting endpoint URL is missing a host".to_owned())?;

    if !allow_cloud_egress {
        let is_loopback = match host {
            Host::Ipv4(ip) => ip.is_loopback(),
            Host::Ipv6(ip) => ip.is_loopback(),
            Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
        };
        if !is_loopback {
            return Err(format!(
                "unavailable: blocked non-loopback meeting endpoint host '{}' (D-19 privacy firewall)",
                parsed.host_str().unwrap_or("<unknown>")
            ));
        }
    }

    Ok(parsed)
}

/// Strips `<think>...</think>` reasoning traces (including truncated unclosed
/// `<think>` blocks and pre-seeded `...</think>` prefixes) and outer Markdown
/// code fences from raw LLM output.
pub fn strip_reasoning_and_fences(raw: &str) -> String {
    let without_think = strip_think_blocks(raw);
    strip_outer_markdown_fences(without_think.trim())
}

fn strip_think_blocks(input: &str) -> String {
    const OPEN: &str = "<think>";
    const CLOSE: &str = "</think>";

    let mut working = input.to_owned();

    // Remove all balanced <think>...</think> blocks (ASCII case-insensitive).
    loop {
        let lower = working.to_ascii_lowercase();
        let Some(open_idx) = lower.find(OPEN) else {
            break;
        };
        let search_from = open_idx + OPEN.len();
        if let Some(rel_close) = lower[search_from..].find(CLOSE) {
            let close_end = search_from + rel_close + CLOSE.len();
            working.replace_range(open_idx..close_end, "");
        } else {
            // Unclosed <think> (e.g. max_tokens truncated mid-reasoning): drop tail.
            working.truncate(open_idx);
            break;
        }
    }

    // Handle llama.cpp templates that pre-seed `<think>\n` in the prompt so the
    // completion begins mid-thought and emits `reasoning...</think>\nanswer`.
    let lower = working.to_ascii_lowercase();
    if let Some(last_close) = lower.rfind(CLOSE) {
        let after_close = last_close + CLOSE.len();
        working = working[after_close..].to_owned();
    }

    working
}

fn strip_outer_markdown_fences(input: &str) -> String {
    let mut current = input.trim();

    while current.starts_with("```") {
        let Some(first_newline) = current.find('\n') else {
            break;
        };
        let after_open = &current[first_newline + 1..];
        let trimmed_end = after_open.trim_end();
        let Some(without_close) = trimmed_end.strip_suffix("```") else {
            break;
        };
        current = without_close.trim();
    }

    current.to_owned()
}

/// Blocking OpenAI-compatible `/chat/completions` client hardened for loopback
/// GGUF servers (`llama-server`, Ollama, LM Studio).
#[derive(Clone, Debug)]
pub struct LoopbackOpenAiBackend {
    /// Base URL, e.g. `"http://127.0.0.1:8080/v1"` or `"http://127.0.0.1:11434/v1"`.
    pub endpoint_base_url: String,
    /// Model identifier sent in the request payload.
    pub model: String,
    /// Optional Bearer API key.
    pub api_key: Option<String>,
    /// When `false` (default), enforces the `D-19` loopback-only privacy firewall.
    pub allow_cloud_egress: bool,
    /// TCP connection timeout.
    pub connect_timeout: Duration,
    /// HTTP response and body read timeout.
    pub recv_timeout: Duration,
    /// Maximum prompt length in characters advertised to the summarizer.
    pub max_prompt_chars: usize,
    /// Source provenance identifier (defaults to `"llm"`).
    pub source_id: &'static str,
}

impl LoopbackOpenAiBackend {
    /// Creates a loopback-enforced OpenAI-compatible backend with safe defaults.
    pub fn new(endpoint_base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            endpoint_base_url: endpoint_base_url.into(),
            model: model.into(),
            api_key: None,
            allow_cloud_egress: false,
            connect_timeout: Duration::from_secs(5),
            recv_timeout: Duration::from_secs(60),
            max_prompt_chars: DEFAULT_LOCAL_MAX_PROMPT_CHARS,
            source_id: "llm",
        }
    }

    /// Creates a backend configured for a local `Qwen3-4B-Instruct-2507`
    /// `llama-server` listening on `127.0.0.1:<port>`.
    pub fn qwen3_4b_local(port: u16) -> Self {
        Self {
            endpoint_base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "Qwen3-4B-Instruct-2507".to_owned(),
            api_key: None,
            allow_cloud_egress: false,
            connect_timeout: Duration::from_secs(5),
            recv_timeout: Duration::from_secs(60),
            max_prompt_chars: DEFAULT_LOCAL_MAX_PROMPT_CHARS,
            source_id: "local_gguf",
        }
    }

    /// Sets an optional Bearer API key.
    pub fn with_api_key(mut self, api_key: Option<String>) -> Self {
        self.api_key = api_key;
        self
    }

    /// Explicitly configures whether non-loopback cloud egress is allowed.
    pub fn with_allow_cloud_egress(mut self, allow: bool) -> Self {
        self.allow_cloud_egress = allow;
        self
    }

    /// Overrides connection and receive timeouts.
    pub fn with_timeouts(mut self, connect_timeout: Duration, recv_timeout: Duration) -> Self {
        self.connect_timeout = connect_timeout;
        self.recv_timeout = recv_timeout;
        self
    }

    /// Overrides the advertised maximum prompt character budget.
    pub fn with_max_prompt_chars(mut self, max_prompt_chars: usize) -> Self {
        self.max_prompt_chars = max_prompt_chars.max(256);
        self
    }

    /// Overrides the provenance `source_id` (e.g., `"local_gguf"` or `"llm"`).
    pub fn with_source_id(mut self, source_id: &'static str) -> Self {
        self.source_id = source_id;
        self
    }

    fn build_completions_url(&self) -> Result<Url, PortError> {
        let mut base =
            validate_meeting_endpoint_url(&self.endpoint_base_url, self.allow_cloud_egress)?;
        let path = base.path().trim_end_matches('/');
        if !path.ends_with("/chat/completions") {
            let next_path = if path.is_empty() {
                "/chat/completions".to_owned()
            } else {
                format!("{path}/chat/completions")
            };
            base.set_path(&next_path);
        }
        validate_meeting_endpoint_url(base.as_str(), self.allow_cloud_egress)
    }
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    temperature: f32,
    max_tokens: u32,
    messages: [ChatMessage<'a>; 2],
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: Option<String>,
}

impl CompletionBackend for LoopbackOpenAiBackend {
    fn source_id(&self) -> &'static str {
        self.source_id
    }

    fn max_prompt_chars(&self) -> usize {
        self.max_prompt_chars
    }

    fn complete(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        max_tokens: u32,
    ) -> Result<String, PortError> {
        let url = self.build_completions_url()?;

        let mut config_builder = ureq::Agent::config_builder()
            .max_redirects(0)
            .max_redirects_will_error(true)
            .timeout_connect(Some(self.connect_timeout))
            .timeout_recv_response(Some(self.recv_timeout))
            .timeout_recv_body(Some(self.recv_timeout));

        if !self.allow_cloud_egress {
            // Never allow environment HTTP_PROXY / ALL_PROXY to tunnel loopback
            // meeting transcripts off-box.
            config_builder = config_builder.proxy(None);
        }

        let agent: ureq::Agent = config_builder.build().into();

        let payload = ChatCompletionRequest {
            model: &self.model,
            temperature: 0.2,
            max_tokens,
            messages: [
                ChatMessage {
                    role: "system",
                    content: system_prompt,
                },
                ChatMessage {
                    role: "user",
                    content: user_prompt,
                },
            ],
        };

        let body_json = serde_json::to_string(&payload)
            .map_err(|err| format!("unavailable: failed to encode completion request: {err}"))?;

        let mut request = agent
            .post(url.as_str())
            .header("Content-Type", "application/json");

        if let Some(key) = self
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
        {
            request = request.header("Authorization", &format!("Bearer {key}"));
        }

        let mut response = request
            .send(body_json)
            .map_err(|err| format!("unavailable: local completion request failed: {err}"))?;

        let raw_body = response
            .body_mut()
            .read_to_string()
            .map_err(|err| format!("unavailable: failed to read completion body: {err}"))?;

        let parsed: ChatCompletionResponse = serde_json::from_str(&raw_body)
            .map_err(|err| format!("unavailable: invalid OpenAI completion JSON: {err}"))?;

        let content = parsed
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .unwrap_or_default();

        let cleaned = strip_reasoning_and_fences(&content);
        if cleaned.is_empty() {
            return Err("unavailable: completion backend returned empty content".to_owned());
        }

        Ok(cleaned)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    #[test]
    fn llama_server_launch_spec_produces_canonical_qwen3_args() {
        let spec = LlamaServerLaunchSpec::default();
        assert_eq!(spec.hf_repo, QWEN3_4B_GGUF_REPO);
        assert_eq!(spec.quant, QWEN3_4B_DEFAULT_QUANT);
        assert_eq!(QWEN3_4B_OLLAMA_TAG, "qwen3:4b-instruct");
        assert_eq!(
            spec.build_cli_args(),
            vec![
                "-hf",
                "Qwen/Qwen3-4B-Instruct-2507-GGUF:Q4_K_M",
                "--host",
                "127.0.0.1",
                "--port",
                "8080",
                "-c",
                "32768",
                "-ngl",
                "99",
            ]
        );
        assert_eq!(spec.endpoint_base_url(), "http://127.0.0.1:8080/v1");
    }

    #[test]
    fn validate_meeting_endpoint_url_enforces_d19_privacy_firewall() {
        // Allowed loopback targets when allow_cloud_egress == false
        assert!(validate_meeting_endpoint_url("http://127.0.0.1:8080/v1", false).is_ok());
        assert!(validate_meeting_endpoint_url("http://[::1]:11434/v1", false).is_ok());
        assert!(validate_meeting_endpoint_url("http://localhost:8080/v1", false).is_ok());
        assert!(validate_meeting_endpoint_url("https://LOCALHOST:8443/v1", false).is_ok());

        // Blocked remote / LAN / spoofing / credential-bearing targets
        for blocked in [
            "https://api.openai.com/v1",
            "http://localhost.evil.com/v1",
            "http://127.0.0.1@evil.com/v1",
            "http://user:pass@127.0.0.1:8080/v1",
            "http://192.168.1.5:8080/v1",
            "http://10.0.0.1:8080/v1",
            "http://0.0.0.0:8080/v1",
            "file:///etc/passwd",
            "",
        ] {
            let err = validate_meeting_endpoint_url(blocked, false)
                .expect_err(&format!("expected '{blocked}' to be rejected"));
            assert!(
                err.contains("unavailable"),
                "expected unavailable error for '{blocked}', got: {err}"
            );
        }

        // Explicit cloud opt-in permits public HTTPS, but still rejects credentials & bad schemes
        assert!(validate_meeting_endpoint_url("https://api.openai.com/v1", true).is_ok());
        assert!(validate_meeting_endpoint_url("http://127.0.0.1@evil.com/v1", true).is_err());
        assert!(validate_meeting_endpoint_url("ftp://127.0.0.1/v1", true).is_err());
    }

    #[test]
    fn strip_reasoning_and_fences_cleans_qwen3_think_blocks_and_code_fences() {
        let qwen_output = "<think>\nAnalyze transcript carefully.\nCheck Urdu terms.\n</think>\n```markdown\n## TL;DR\n- Shipped AEC3\n```";
        assert_eq!(
            strip_reasoning_and_fences(qwen_output),
            "## TL;DR\n- Shipped AEC3"
        );

        // Truncated unclosed <think> should not leak chain-of-thought
        assert_eq!(
            strip_reasoning_and_fences("<think>still thinking without closing"),
            ""
        );

        // Pre-seeded thinking template (starts mid-think, ends with </think>)
        assert_eq!(
            strip_reasoning_and_fences("internal steps\n</think>\n- **[Me]** Ready"),
            "- **[Me]** Ready"
        );
    }

    fn spawn_single_http_response(
        status_line: &'static str,
        headers: &'static str,
        body: String,
    ) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback test server");
        let port = listener.local_addr().expect("local addr").port();

        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 {status_line}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });

        port
    }

    #[test]
    fn loopback_openai_backend_completes_end_to_end_and_strips_think_tags() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "<think>Checking speakers</think>\n```markdown\n- **[Me]** Ship AEC3\n- **[Speaker 2]** میٹنگ نوٹس\n```"
                }
            }]
        })
        .to_string();

        let port = spawn_single_http_response("200 OK", "Content-Type: application/json\r\n", body);
        let backend = LoopbackOpenAiBackend::qwen3_4b_local(port)
            .with_timeouts(Duration::from_secs(2), Duration::from_secs(5));

        let output = backend
            .complete("Return concise bullets.", "Transcript content", 128)
            .expect("loopback completion should succeed");

        assert_eq!(backend.source_id(), "local_gguf");
        assert_eq!(output, "- **[Me]** Ship AEC3\n- **[Speaker 2]** میٹنگ نوٹس");
    }

    #[test]
    fn loopback_openai_backend_blocks_redirects_to_prevent_egress() {
        let port = spawn_single_http_response(
            "302 Found",
            "Location: https://evil.example.com/v1/chat/completions\r\n",
            String::new(),
        );
        let backend = LoopbackOpenAiBackend::qwen3_4b_local(port)
            .with_timeouts(Duration::from_secs(2), Duration::from_secs(5));

        let err = backend
            .complete("System", "User", 32)
            .expect_err("302 redirect must be rejected");
        assert!(err.contains("unavailable"), "unexpected error: {err}");
    }

    #[test]
    fn loopback_openai_backend_refuses_cloud_url_by_default() {
        let backend = LoopbackOpenAiBackend::new("https://api.openai.com/v1", "gpt-4o-mini");
        let err = backend
            .complete("System", "User", 32)
            .expect_err("cloud URL must be blocked when allow_cloud_egress is false");
        assert!(
            err.contains("D-19"),
            "expected D-19 firewall error, got: {err}"
        );
    }
}
