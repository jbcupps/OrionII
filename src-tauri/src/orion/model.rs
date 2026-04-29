use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::orion::ethics::EthicsOverlay;
use crate::orion::identity::IdentityState;
use crate::orion::sao::SaoClientConfig;

#[derive(Clone, Debug)]
pub struct ModelPrompt {
    pub system_prompt: String,
    pub user_query: String,
    pub context: String,
}

pub type ModelResult<T> = Result<T, ModelError>;

#[async_trait]
pub trait ModelProvider: Send + Sync {
    async fn consult_id(
        &self,
        identity: &IdentityState,
        query: &str,
        context: &str,
    ) -> ModelResult<String>;
    async fn generate_ego_response(
        &self,
        prompt: &ModelPrompt,
        ethics: &EthicsOverlay,
    ) -> ModelResult<String>;
}

#[derive(Clone, Debug, Error)]
pub enum ModelError {
    #[error("model runtime is unavailable: {0}")]
    RuntimeUnavailable(String),
    #[error("model HTTP call failed: {0}")]
    HttpFailure(String),
    #[error("model response was invalid: {0}")]
    InvalidResponse(String),
    #[error("model call timed out after {0}ms")]
    Timeout(u64),
    #[error("model prompt was rejected: {0}")]
    PromptRejected(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfig {
    pub provider: ModelProviderKind,
    pub ollama_base_url: String,
    pub id_model: String,
    pub ego_model: String,
    /// Optional sampling temperature override for the Id role. When `None`, OrionII
    /// omits the field from the SAO proxy request and lets the upstream model use its
    /// default. Required for GPT-5.x and reasoning models that reject any custom value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_temperature: Option<f32>,
    /// Optional sampling temperature override for the Ego role. Same semantics as
    /// `id_temperature`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ego_temperature: Option<f32>,
    pub timeout_ms: u64,
    /// Provider key SAO uses to dispatch (`openai`, `anthropic`, `ollama`).
    /// Only meaningful when `provider == SaoProxyWithFallback`.
    pub sao_provider: String,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: ModelProviderKind::OllamaWithFallback,
            ollama_base_url: "http://127.0.0.1:11434".to_string(),
            id_model: "llama3.2".to_string(),
            ego_model: "llama3.2".to_string(),
            id_temperature: None,
            ego_temperature: None,
            timeout_ms: 20_000,
            sao_provider: "ollama".to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelProviderKind {
    Deterministic,
    OllamaWithFallback,
    /// Route /api/llm/generate calls through SAO. Falls back to deterministic on transport
    /// failure so the entity can still respond if SAO is briefly unreachable.
    SaoProxyWithFallback,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelRole {
    Id,
    Ego,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelCallState {
    Healthy,
    Fallback,
    Degraded,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCallStatus {
    pub role: ModelRole,
    pub provider: ModelProviderKind,
    pub state: ModelCallState,
    pub model: String,
    pub message: Option<String>,
}

#[derive(Default)]
pub struct DeterministicModelProvider;

#[async_trait]
impl ModelProvider for DeterministicModelProvider {
    async fn consult_id(
        &self,
        identity: &IdentityState,
        query: &str,
        context: &str,
    ) -> ModelResult<String> {
        Ok(format!(
            "{} remains {}. Query signal: '{}'. Context available: {}.",
            identity.personality.name,
            identity.personality.stance,
            query,
            if context.is_empty() { "no" } else { "yes" }
        ))
    }

    async fn generate_ego_response(
        &self,
        prompt: &ModelPrompt,
        ethics: &EthicsOverlay,
    ) -> ModelResult<String> {
        let context_line = if prompt.context.is_empty() {
            "I do not have matching local document context yet.".to_string()
        } else {
            format!("Local context considered: {}", prompt.context)
        };

        Ok(format!(
            "Orion is operating as a persistent local companion. {}\n\nI heard: \"{}\"\n\nSystem stance: {}\n\nEthics scaffold: {}",
            context_line,
            prompt.user_query,
            prompt.system_prompt,
            ethics.guidance.join(" ")
        ))
    }
}

pub struct ModelRouter {
    config: ModelConfig,
    deterministic: DeterministicModelProvider,
    ollama: OllamaModelProvider,
    sao_proxy: Option<SaoProxyProvider>,
    statuses: Mutex<Vec<ModelCallStatus>>,
}

impl ModelRouter {
    pub fn new(config: ModelConfig) -> Self {
        Self {
            ollama: OllamaModelProvider::new(config.clone()),
            sao_proxy: None,
            config,
            deterministic: DeterministicModelProvider,
            statuses: Mutex::new(Vec::new()),
        }
    }

    pub fn with_sao_proxy(config: ModelConfig, sao: SaoClientConfig) -> Self {
        let sao_proxy = Some(SaoProxyProvider::new(config.clone(), sao));
        Self {
            ollama: OllamaModelProvider::new(config.clone()),
            sao_proxy,
            config,
            deterministic: DeterministicModelProvider,
            statuses: Mutex::new(Vec::new()),
        }
    }

    pub fn config(&self) -> &ModelConfig {
        &self.config
    }

    #[allow(dead_code)]
    pub fn clear_statuses(&self) {
        if let Ok(mut statuses) = self.statuses.lock() {
            statuses.clear();
        }
    }

    pub fn statuses(&self) -> Vec<ModelCallStatus> {
        self.statuses
            .lock()
            .map(|statuses| statuses.clone())
            .unwrap_or_default()
    }

    fn record(&self, status: ModelCallStatus) {
        if let Ok(mut statuses) = self.statuses.lock() {
            statuses.push(status);
        }
    }

    async fn fallback_id(
        &self,
        identity: &IdentityState,
        query: &str,
        context: &str,
    ) -> ModelResult<String> {
        let text = self
            .deterministic
            .consult_id(identity, query, context)
            .await?;
        self.record(ModelCallStatus {
            role: ModelRole::Id,
            provider: ModelProviderKind::Deterministic,
            state: ModelCallState::Fallback,
            model: "deterministic".to_string(),
            message: Some("using deterministic Id fallback".to_string()),
        });
        Ok(text)
    }

    async fn fallback_ego(
        &self,
        prompt: &ModelPrompt,
        ethics: &EthicsOverlay,
    ) -> ModelResult<String> {
        let text = self
            .deterministic
            .generate_ego_response(prompt, ethics)
            .await?;
        self.record(ModelCallStatus {
            role: ModelRole::Ego,
            provider: ModelProviderKind::Deterministic,
            state: ModelCallState::Fallback,
            model: "deterministic".to_string(),
            message: Some("using deterministic Ego fallback".to_string()),
        });
        Ok(text)
    }
}

impl Default for ModelRouter {
    fn default() -> Self {
        Self::new(ModelConfig::default())
    }
}

#[async_trait]
impl ModelProvider for ModelRouter {
    async fn consult_id(
        &self,
        identity: &IdentityState,
        query: &str,
        context: &str,
    ) -> ModelResult<String> {
        match self.config.provider {
            ModelProviderKind::Deterministic => self.fallback_id(identity, query, context).await,
            ModelProviderKind::SaoProxyWithFallback => {
                let provider_kind = ModelProviderKind::SaoProxyWithFallback;
                let result = match self.sao_proxy.as_ref() {
                    Some(p) => p.consult_id(identity, query, context).await,
                    None => Err(ModelError::RuntimeUnavailable(
                        "SAO proxy not initialized".to_string(),
                    )),
                };
                match result {
                    Ok(text) => {
                        self.record(ModelCallStatus {
                            role: ModelRole::Id,
                            provider: provider_kind,
                            state: ModelCallState::Healthy,
                            model: self.config.id_model.clone(),
                            message: None,
                        });
                        Ok(text)
                    }
                    Err(error) => {
                        self.record(ModelCallStatus {
                            role: ModelRole::Id,
                            provider: provider_kind,
                            state: ModelCallState::Degraded,
                            model: self.config.id_model.clone(),
                            message: Some(error.to_string()),
                        });
                        self.fallback_id(identity, query, context).await
                    }
                }
            }
            ModelProviderKind::OllamaWithFallback => {
                match self.ollama.consult_id(identity, query, context).await {
                    Ok(text) => {
                        self.record(ModelCallStatus {
                            role: ModelRole::Id,
                            provider: ModelProviderKind::OllamaWithFallback,
                            state: ModelCallState::Healthy,
                            model: self.config.id_model.clone(),
                            message: None,
                        });
                        Ok(text)
                    }
                    Err(error) => {
                        self.record(ModelCallStatus {
                            role: ModelRole::Id,
                            provider: ModelProviderKind::OllamaWithFallback,
                            state: ModelCallState::Degraded,
                            model: self.config.id_model.clone(),
                            message: Some(error.to_string()),
                        });
                        self.fallback_id(identity, query, context).await
                    }
                }
            }
        }
    }

    async fn generate_ego_response(
        &self,
        prompt: &ModelPrompt,
        ethics: &EthicsOverlay,
    ) -> ModelResult<String> {
        match self.config.provider {
            ModelProviderKind::Deterministic => self.fallback_ego(prompt, ethics).await,
            ModelProviderKind::SaoProxyWithFallback => {
                let provider_kind = ModelProviderKind::SaoProxyWithFallback;
                let result = match self.sao_proxy.as_ref() {
                    Some(p) => p.generate_ego_response(prompt, ethics).await,
                    None => Err(ModelError::RuntimeUnavailable(
                        "SAO proxy not initialized".to_string(),
                    )),
                };
                match result {
                    Ok(text) => {
                        self.record(ModelCallStatus {
                            role: ModelRole::Ego,
                            provider: provider_kind,
                            state: ModelCallState::Healthy,
                            model: self.config.ego_model.clone(),
                            message: None,
                        });
                        Ok(text)
                    }
                    Err(error) => {
                        self.record(ModelCallStatus {
                            role: ModelRole::Ego,
                            provider: provider_kind,
                            state: ModelCallState::Degraded,
                            model: self.config.ego_model.clone(),
                            message: Some(error.to_string()),
                        });
                        self.fallback_ego(prompt, ethics).await
                    }
                }
            }
            ModelProviderKind::OllamaWithFallback => {
                match self.ollama.generate_ego_response(prompt, ethics).await {
                    Ok(text) => {
                        self.record(ModelCallStatus {
                            role: ModelRole::Ego,
                            provider: ModelProviderKind::OllamaWithFallback,
                            state: ModelCallState::Healthy,
                            model: self.config.ego_model.clone(),
                            message: None,
                        });
                        Ok(text)
                    }
                    Err(error) => {
                        self.record(ModelCallStatus {
                            role: ModelRole::Ego,
                            provider: ModelProviderKind::OllamaWithFallback,
                            state: ModelCallState::Degraded,
                            model: self.config.ego_model.clone(),
                            message: Some(error.to_string()),
                        });
                        self.fallback_ego(prompt, ethics).await
                    }
                }
            }
        }
    }
}

pub struct OllamaModelProvider {
    config: ModelConfig,
    client: reqwest::Client,
}

impl OllamaModelProvider {
    pub fn new(config: ModelConfig) -> Self {
        let timeout = Duration::from_millis(config.timeout_ms);
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self { config, client }
    }

    async fn generate(
        &self,
        role: ModelRole,
        model: &str,
        system: String,
        prompt: String,
        temperature: Option<f32>,
    ) -> ModelResult<String> {
        if prompt.trim().is_empty() {
            return Err(ModelError::PromptRejected("empty prompt".to_string()));
        }

        let url = format!(
            "{}/api/generate",
            self.config.ollama_base_url.trim_end_matches('/')
        );
        let request = OllamaGenerateRequest {
            model,
            system: &system,
            prompt: &prompt,
            stream: false,
            options: OllamaOptions { temperature },
        };

        let response = self
            .client
            .post(url)
            .json(&request)
            .send()
            .await
            .map_err(|error| map_reqwest_error(error, self.config.timeout_ms))?;

        let status = response.status();
        if !status.is_success() {
            return Err(ModelError::HttpFailure(format!(
                "Ollama {:?} request returned {}",
                role, status
            )));
        }

        let body = response
            .text()
            .await
            .map_err(|error| ModelError::HttpFailure(error.to_string()))?;

        parse_ollama_generate_response(&body)
    }
}

#[async_trait]
impl ModelProvider for OllamaModelProvider {
    async fn consult_id(
        &self,
        identity: &IdentityState,
        query: &str,
        context: &str,
    ) -> ModelResult<String> {
        let system = format!(
            "You are Orion's Id layer. Speak as prompt-time personality guidance only. Identity version: {}. Drives: {}.",
            identity.version,
            identity.drives.join(", ")
        );
        let prompt = format!(
            "Worker query: {}\n\nLocal context: {}\n\nReturn concise personality, tone, and values guidance for Ego.",
            query,
            if context.is_empty() { "none" } else { context }
        );

        self.generate(
            ModelRole::Id,
            &self.config.id_model,
            system,
            prompt,
            self.config.id_temperature,
        )
        .await
    }

    async fn generate_ego_response(
        &self,
        prompt: &ModelPrompt,
        ethics: &EthicsOverlay,
    ) -> ModelResult<String> {
        let user_prompt = format!(
            "User query: {}\n\nLocal context: {}\n\nEthics guidance: {}",
            prompt.user_query,
            prompt.context,
            ethics.guidance.join(" ")
        );

        self.generate(
            ModelRole::Ego,
            &self.config.ego_model,
            prompt.system_prompt.clone(),
            user_prompt,
            self.config.ego_temperature,
        )
        .await
    }
}

#[derive(Serialize)]
struct OllamaGenerateRequest<'a> {
    model: &'a str,
    system: &'a str,
    prompt: &'a str,
    stream: bool,
    options: OllamaOptions,
}

#[derive(Serialize)]
struct OllamaOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Deserialize)]
struct OllamaGenerateResponse {
    response: Option<String>,
    error: Option<String>,
}

pub fn parse_ollama_generate_response(body: &str) -> ModelResult<String> {
    let parsed: OllamaGenerateResponse = serde_json::from_str(body)
        .map_err(|error| ModelError::InvalidResponse(error.to_string()))?;

    if let Some(error) = parsed.error {
        return Err(ModelError::RuntimeUnavailable(error));
    }

    let response = parsed
        .response
        .ok_or_else(|| ModelError::InvalidResponse("missing response field".to_string()))?;

    if response.trim().is_empty() {
        return Err(ModelError::InvalidResponse(
            "empty response field".to_string(),
        ));
    }

    Ok(response)
}

fn map_reqwest_error(error: reqwest::Error, timeout_ms: u64) -> ModelError {
    if error.is_timeout() {
        ModelError::Timeout(timeout_ms)
    } else if error.is_connect() {
        ModelError::RuntimeUnavailable(error.to_string())
    } else {
        ModelError::HttpFailure(error.to_string())
    }
}

/// Routes Id/Ego prompts through SAO's hosted LLM proxy.
/// SAO holds the provider keys; this client only carries the entity bearer token.
pub struct SaoProxyProvider {
    config: ModelConfig,
    sao: SaoClientConfig,
    client: reqwest::Client,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SaoProxyRequest<'a> {
    provider: &'a str,
    model: &'a str,
    system: &'a str,
    prompt: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    role: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaoProxyResponse {
    text: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

impl SaoProxyProvider {
    pub fn new(config: ModelConfig, sao: SaoClientConfig) -> Self {
        let timeout = std::time::Duration::from_millis(config.timeout_ms);
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            sao,
            client,
        }
    }

    async fn call(
        &self,
        role: ModelRole,
        model: &str,
        system: String,
        prompt: String,
        temperature: Option<f32>,
    ) -> ModelResult<String> {
        if prompt.trim().is_empty() {
            return Err(ModelError::PromptRejected("empty prompt".to_string()));
        }

        let url = format!("{}/api/llm/generate", self.sao.base_url);
        let role_str = match role {
            ModelRole::Id => "id",
            ModelRole::Ego => "ego",
        };

        let body = SaoProxyRequest {
            provider: &self.config.sao_provider,
            model,
            system: &system,
            prompt: &prompt,
            temperature,
            role: role_str,
        };

        let response = self
            .client
            .post(url)
            .bearer_auth(&self.sao.bearer_token)
            .json(&body)
            .send()
            .await
            .map_err(|error| map_reqwest_error(error, self.config.timeout_ms))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| ModelError::HttpFailure(error.to_string()))?;

        if !status.is_success() {
            return Err(ModelError::HttpFailure(format!(
                "SAO LLM proxy returned {status}: {text}"
            )));
        }

        let parsed: SaoProxyResponse =
            serde_json::from_str(&text).map_err(|e| ModelError::InvalidResponse(e.to_string()))?;
        if let Some(err) = parsed.error {
            return Err(ModelError::RuntimeUnavailable(err));
        }
        parsed
            .text
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ModelError::InvalidResponse("missing text field".to_string()))
    }
}

#[async_trait]
impl ModelProvider for SaoProxyProvider {
    async fn consult_id(
        &self,
        identity: &IdentityState,
        query: &str,
        context: &str,
    ) -> ModelResult<String> {
        let system = format!(
            "You are Orion's Id layer. Identity version: {}. Drives: {}.",
            identity.version,
            identity.drives.join(", ")
        );
        let prompt = format!(
            "Worker query: {}\n\nLocal context: {}\n\nReturn concise personality, tone, and values guidance for Ego.",
            query,
            if context.is_empty() { "none" } else { context }
        );
        self.call(
            ModelRole::Id,
            &self.config.id_model,
            system,
            prompt,
            self.config.id_temperature,
        )
        .await
    }

    async fn generate_ego_response(
        &self,
        prompt: &ModelPrompt,
        ethics: &EthicsOverlay,
    ) -> ModelResult<String> {
        let user_prompt = format!(
            "User query: {}\n\nLocal context: {}\n\nEthics guidance: {}",
            prompt.user_query,
            prompt.context,
            ethics.guidance.join(" ")
        );
        self.call(
            ModelRole::Ego,
            &self.config.ego_model,
            prompt.system_prompt.clone(),
            user_prompt,
            self.config.ego_temperature,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orion::identity::IdentityState;

    #[test]
    fn parses_ollama_generate_response() {
        let parsed = parse_ollama_generate_response(r#"{"response":"hello from ollama"}"#).unwrap();

        assert_eq!(parsed, "hello from ollama");
    }

    #[test]
    fn rejects_invalid_ollama_response() {
        let error = parse_ollama_generate_response(r#"{"done":true}"#).unwrap_err();

        assert!(matches!(error, ModelError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn router_falls_back_when_ollama_is_unavailable() {
        let config = ModelConfig {
            ollama_base_url: "http://127.0.0.1:9".to_string(),
            timeout_ms: 50,
            ..ModelConfig::default()
        };
        let router = ModelRouter::new(config);
        let identity = IdentityState::bootstrap();

        let text = router.consult_id(&identity, "hello", "").await.unwrap();
        let statuses = router.statuses();

        assert!(text.contains("Orion remains"));
        assert!(statuses
            .iter()
            .any(|status| status.state == ModelCallState::Degraded));
        assert!(statuses
            .iter()
            .any(|status| status.state == ModelCallState::Fallback));
    }

    #[test]
    fn config_keeps_id_and_ego_models_separate() {
        let config = ModelConfig {
            id_model: "id-model".to_string(),
            ego_model: "ego-model".to_string(),
            ..ModelConfig::default()
        };
        let router = ModelRouter::new(config);

        assert_eq!(router.config().id_model, "id-model");
        assert_eq!(router.config().ego_model, "ego-model");
    }
}
