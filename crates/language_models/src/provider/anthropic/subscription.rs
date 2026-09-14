use super::{
    AnthropicLanguageModelProvider, AnthropicModel, Authentication, DEFAULT_FAST_MODEL_PREFIXES,
    DEFAULT_MODEL_PREFIXES, claude_fast_mode_confirmation, models_with_overrides,
    pick_preferred_model,
};
use anyhow::{Context as _, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use credentials_provider::CredentialsProvider;
use futures::{AsyncReadExt as _, FutureExt as _, future::Shared, lock::Mutex};
use gpui::{App, AppContext as _, Context, Entity, SharedString, Task, Window};
use http_client::{
    AsyncBody, CustomHeaders, HttpClient, HttpRequestExt as _, Request,
    http::{HeaderName, HeaderValue, StatusCode},
};
use language_model::{
    AuthenticateError, FastModeConfirmation, IconOrSvg, InlineDescription, InlineProviderSettings,
    LanguageModel, LanguageModelId, LanguageModelProvider, LanguageModelProviderId,
    LanguageModelProviderName, LanguageModelProviderState, ProviderSettingsView, RateLimiter,
};
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use ui::{ConfiguredApiCard, prelude::*};
use util::ResultExt as _;

pub(super) const PROVIDER_ID: LanguageModelProviderId =
    LanguageModelProviderId::new("claude-subscription");
pub(super) const PROVIDER_NAME: LanguageModelProviderName =
    LanguageModelProviderName::new("Claude Subscription");
pub(super) const OAUTH_BETA: &str = "oauth-2025-04-20";
const AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
// The public native-app registration accepts localhost callbacks with ephemeral ports.
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CREDENTIALS_KEY: &str = "https://claude.ai/zed-subscription";
const SCOPES: &str = "user:profile user:inference";

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn post_token(
    http_client: &dyn HttpClient,
    body: serde_json::Value,
) -> Result<(StatusCode, Vec<u8>)> {
    let request = Request::post(TOKEN_URL)
        .timeout(Duration::from_secs(30))
        .header("Content-Type", "application/json")
        .body(AsyncBody::from(serde_json::to_string(&body)?))?;
    let mut response = http_client.send(request).await?;
    let mut body = Vec::new();
    response.body_mut().read_to_end(&mut body).await?;
    Ok((response.status(), body))
}

type RefreshTask = Shared<Task<Result<Credentials, Arc<anyhow::Error>>>>;

/// The refresh token was rejected and the stored credentials have been cleared.
#[derive(Debug)]
pub(super) struct SessionExpired {
    pub(super) status: StatusCode,
}

impl std::fmt::Display for SessionExpired {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Claude session expired (HTTP {})", self.status)
    }
}

impl std::error::Error for SessionExpired {}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Credentials {
    access_token: String,
    refresh_token: String,
    expires_at: u64,
    email: Option<String>,
}

impl Credentials {
    pub(super) fn headers(
        &self,
        custom_headers: &CustomHeaders,
    ) -> Result<Vec<(HeaderName, HeaderValue)>> {
        let mut authorization = HeaderValue::from_str(&format!("Bearer {}", self.access_token))?;
        authorization.set_sensitive(true);
        let mut headers = custom_headers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<Vec<_>>();
        headers.push((http_client::http::header::AUTHORIZATION, authorization));
        Ok(headers)
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    account: Option<Account>,
}

#[derive(Deserialize)]
struct Account {
    email_address: Option<String>,
}

pub struct State {
    pub(super) credentials: Option<Credentials>,
    models: Vec<anthropic::Model>,
    http_client: Arc<dyn HttpClient>,
    credentials_provider: Arc<dyn CredentialsProvider>,
    // Serialize keychain writes so sign-out cannot be undone by an in-flight refresh.
    credential_lock: Arc<Mutex<()>>,
    generation: u64,
    load_task: Option<Shared<Task<Result<(), Arc<anyhow::Error>>>>>,
    refresh_task: Option<RefreshTask>,
    sign_in_task: Option<Task<()>>,
    persisting: bool,
    error: Option<SharedString>,
}

impl State {
    fn new(
        http_client: Arc<dyn HttpClient>,
        credentials_provider: Arc<dyn CredentialsProvider>,
        cx: &mut Context<Self>,
    ) -> Self {
        let credential_lock = Arc::new(Mutex::new(()));
        let load_task = cx
            .spawn({
                let credential_lock = credential_lock.clone();
                let credentials_provider = credentials_provider.clone();
                async move |this, cx| {
                    let result = async {
                        let guard = credential_lock.lock().await;
                        let stored = credentials_provider
                            .read_credentials(CREDENTIALS_KEY, cx)
                            .await?;
                        let credentials = stored
                            .map(|(_, bytes)| serde_json::from_slice::<Credentials>(&bytes))
                            .transpose()?;
                        let models_task = this.update(cx, |state, cx| {
                            if state.generation != 0 {
                                return None;
                            }
                            state.credentials = credentials;
                            state.credentials.is_some().then(|| state.fetch_models(cx))
                        })?;
                        drop(guard);
                        if let Some(task) = models_task {
                            task.await?;
                        }
                        Ok::<_, anyhow::Error>(())
                    }
                    .await;
                    this.update(cx, |state, cx| {
                        if state.generation == 0 {
                            state.error = result.as_ref().err().map(|error| {
                                format!("Could not load Claude subscription: {error:#}").into()
                            });
                        }
                        state.load_task = None;
                        cx.notify();
                    })?;
                    result.map_err(Arc::new)
                }
            })
            .shared();
        Self {
            credentials: None,
            models: Vec::new(),
            http_client,
            credentials_provider,
            credential_lock,
            generation: 0,
            load_task: Some(load_task),
            refresh_task: None,
            sign_in_task: None,
            persisting: false,
            error: None,
        }
    }

    pub(super) fn fresh_credentials(&mut self, cx: &mut Context<Self>) -> RefreshTask {
        let Some(credentials) = self.credentials.clone() else {
            return Task::ready(Err(Arc::new(anyhow!(
                "Sign in to Claude Subscription in Settings > AI > LLM Providers."
            ))))
            .shared();
        };
        if credentials.expires_at > now_secs().saturating_add(300) {
            return Task::ready(Ok(credentials)).shared();
        }
        if let Some(task) = &self.refresh_task {
            return task.clone();
        }
        let generation = self.generation;
        let http_client = self.http_client.clone();
        let credential_lock = self.credential_lock.clone();
        let credentials_provider = self.credentials_provider.clone();
        let task = cx
            .spawn(async move |this, cx| {
                let result = async {
                    let (status, body) = post_token(
                        http_client.as_ref(),
                        serde_json::json!({
                            "grant_type": "refresh_token", "client_id": CLIENT_ID,
                            "refresh_token": credentials.refresh_token,
                        }),
                    )
                    .await?;
                    // Taken only after the network round trip so a sign-in that lands
                    // meanwhile does not wait on it.
                    let _guard = credential_lock.lock().await;
                    // A sign-in or sign-out that finished meanwhile owns the credentials now.
                    let still_current = this.read_with(cx, |state, _| {
                        state
                            .credentials
                            .as_ref()
                            .map(|current| &current.refresh_token)
                            == Some(&credentials.refresh_token)
                    })?;
                    if !still_current {
                        bail!("Claude sign-in changed during token refresh");
                    }
                    if matches!(status.as_u16(), 400 | 401 | 403) {
                        this.update(cx, |state, cx| {
                            state.credentials = None;
                            state.models.clear();
                            state.error =
                                Some("Your Claude session expired. Sign in again.".into());
                            cx.notify();
                        })?;
                        // The session-expired error must win over a keychain cleanup failure.
                        credentials_provider
                            .delete_credentials(CREDENTIALS_KEY, cx)
                            .await
                            .log_err();
                        return Err(SessionExpired { status }.into());
                    }
                    if !status.is_success() {
                        bail!("Claude token refresh failed (HTTP {status})");
                    }
                    let tokens: TokenResponse = serde_json::from_slice(&body)?;
                    let refreshed = Credentials {
                        access_token: tokens.access_token,
                        refresh_token: tokens.refresh_token.unwrap_or(credentials.refresh_token),
                        expires_at: now_secs().saturating_add(tokens.expires_in),
                        email: tokens
                            .account
                            .and_then(|account| account.email_address)
                            .or(credentials.email),
                    };
                    credentials_provider
                        .write_credentials(
                            CREDENTIALS_KEY,
                            "Bearer",
                            &serde_json::to_vec(&refreshed)?,
                            cx,
                        )
                        .await?;
                    this.update(cx, |state, cx| {
                        // While the lock is held only a sign-out can change the credentials,
                        // and its keychain delete runs after this write.
                        if state.credentials.is_none() {
                            bail!("Claude signed out during token refresh");
                        }
                        state.credentials = Some(refreshed.clone());
                        cx.notify();
                        Ok(())
                    })??;
                    Ok(refreshed)
                }
                .await;
                this.update(cx, |state, _| {
                    if state.generation == generation {
                        state.refresh_task = None;
                    }
                })?;
                result.map_err(Arc::new)
            })
            .shared();
        self.refresh_task = Some(task.clone());
        task
    }

    fn fetch_models(&mut self, cx: &mut Context<Self>) -> Task<Result<()>> {
        let credentials = self.fresh_credentials(cx);
        let http_client = self.http_client.clone();
        let api_url = AnthropicLanguageModelProvider::api_url(cx);
        let custom_headers = AnthropicLanguageModelProvider::settings(cx)
            .custom_headers
            .clone();
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            let credentials = credentials.await.map_err(|error| anyhow!("{error:#}"))?;
            let mut headers = credentials.headers(&custom_headers)?;
            headers.push((
                HeaderName::from_static("anthropic-beta"),
                HeaderValue::from_static(OAUTH_BETA),
            ));
            let models = anthropic::list_models(
                http_client.as_ref(),
                &api_url,
                "",
                &CustomHeaders::new(headers),
            )
            .await
            .map_err(|error| anthropic::completion_error_from_anthropic(error, PROVIDER_NAME))?;
            this.update(cx, |state, cx| {
                if state.generation == generation {
                    state.models = models;
                    cx.notify();
                }
            })
        })
    }

    fn sign_in(&mut self, cx: &mut Context<Self>) {
        if self.sign_in_task.is_some() || self.persisting {
            return;
        }
        self.generation += 1;
        // A refresh that finishes after this bump skips its own cleanup, so drop it here.
        self.refresh_task = None;
        self.error = None;
        let generation = self.generation;
        let http_client = self.http_client.clone();
        let credentials_provider = self.credentials_provider.clone();
        let credential_lock = self.credential_lock.clone();
        self.sign_in_task = Some(cx.spawn(async move |this, cx| {
            let result = async {
                let (redirect_uri, callback) =
                    oauth_callback_server::start_oauth_callback_server_with_config(
                        oauth_callback_server::OAuthCallbackServerConfig {
                            host: "localhost",
                            preferred_port: 0,
                            fallback_port: None,
                            path: "/callback",
                        },
                    )?;
                let mut random = [0u8; 32];
                rand::rng().fill_bytes(&mut random);
                let verifier = URL_SAFE_NO_PAD.encode(random);
                rand::rng().fill_bytes(&mut random);
                let expected_state = URL_SAFE_NO_PAD.encode(random);
                let mut url = url::Url::parse(AUTHORIZE_URL)?;
                url.query_pairs_mut()
                    .append_pair("code", "true")
                    .append_pair("client_id", CLIENT_ID)
                    .append_pair("response_type", "code")
                    .append_pair("redirect_uri", &redirect_uri)
                    .append_pair("scope", SCOPES)
                    .append_pair(
                        "code_challenge",
                        &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
                    )
                    .append_pair("code_challenge_method", "S256")
                    .append_pair("state", &expected_state);
                cx.update(|cx| cx.open_url(url.as_str()));
                // The callback server drops its sender on timeout as well as on cancel.
                let callback = callback
                    .await
                    .context("Claude sign-in was cancelled or timed out")??;
                if callback.state != expected_state {
                    bail!("Claude OAuth state mismatch");
                }
                // Once the authorization code arrives, cancelling must not drop the task before the credentials are saved.
                this.update(cx, |state, cx| {
                    state.persisting = true;
                    cx.notify();
                })?;
                let (status, body) = post_token(
                    http_client.as_ref(),
                    serde_json::json!({
                        "grant_type": "authorization_code", "client_id": CLIENT_ID,
                        "code": callback.code, "state": expected_state,
                        "redirect_uri": redirect_uri, "code_verifier": verifier,
                    }),
                )
                .await?;
                if !status.is_success() {
                    bail!("Claude sign-in failed (HTTP {status})");
                }
                let tokens: TokenResponse = serde_json::from_slice(&body)?;
                let credentials = Credentials {
                    access_token: tokens.access_token,
                    refresh_token: tokens
                        .refresh_token
                        .context("Claude did not return a refresh token")?,
                    expires_at: now_secs().saturating_add(tokens.expires_in),
                    email: tokens.account.and_then(|account| account.email_address),
                };
                let guard = credential_lock.lock().await;
                if this.read_with(cx, |state, _| state.generation)? != generation {
                    bail!("Claude sign-in was cancelled");
                }
                credentials_provider
                    .write_credentials(
                        CREDENTIALS_KEY,
                        "Bearer",
                        &serde_json::to_vec(&credentials)?,
                        cx,
                    )
                    .await?;
                let models = this.update(cx, |state, cx| {
                    if state.generation != generation {
                        return None;
                    }
                    state.credentials = Some(credentials);
                    Some(state.fetch_models(cx))
                })?;
                drop(guard);
                let Some(models) = models else {
                    bail!("Claude sign-in was cancelled");
                };
                models.await
            }
            .await;
            this.update(cx, |state, cx| {
                if state.generation == generation {
                    state.error = result.err().map(|error| format!("{error:#}").into());
                    state.sign_in_task = None;
                    state.persisting = false;
                    cx.notify();
                }
            })
            .log_err();
        }));
        cx.notify();
    }

    fn cancel_sign_in(&mut self, cx: &mut Context<Self>) {
        if self.persisting {
            return;
        }
        self.generation += 1;
        self.sign_in_task = None;
        self.refresh_task = None;
        cx.notify();
    }

    fn sign_out(&mut self, cx: &mut Context<Self>) -> Task<Result<()>> {
        self.generation += 1;
        self.credentials = None;
        self.models.clear();
        self.error = None;
        // Dropping a sign-in mid-write could leave the keychain half-updated; let it finish first.
        let sign_in = self.sign_in_task.take().filter(|_| self.persisting);
        self.persisting = true;
        let refresh = self.refresh_task.take();
        let credentials_provider = self.credentials_provider.clone();
        let credential_lock = self.credential_lock.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            if let Some(task) = sign_in {
                task.await;
            }
            if let Some(task) = refresh {
                // A sign-out intentionally invalidates a pending refresh.
                if let Err(error) = task.await {
                    log::debug!("Claude refresh ended during sign-out: {error:#}");
                }
            }
            let _guard = credential_lock.lock().await;
            let result = credentials_provider
                .delete_credentials(CREDENTIALS_KEY, cx)
                .await;
            this.update(cx, |state, cx| {
                state.persisting = false;
                state.error = result
                    .as_ref()
                    .err()
                    .map(|error| format!("Could not remove Claude credentials: {error:#}").into());
                cx.notify();
            })?;
            result
        })
    }
}

pub struct ClaudeSubscriptionProvider {
    state: Entity<State>,
}

impl ClaudeSubscriptionProvider {
    pub fn new(
        http_client: Arc<dyn HttpClient>,
        credentials_provider: Arc<dyn CredentialsProvider>,
        cx: &mut App,
    ) -> Self {
        Self {
            state: cx.new(|cx| State::new(http_client, credentials_provider, cx)),
        }
    }

    fn create_model(&self, mut model: anthropic::Model, cx: &App) -> Arc<dyn LanguageModel> {
        if !model
            .extra_beta_headers
            .iter()
            .any(|header| header == OAUTH_BETA)
        {
            model.extra_beta_headers.push(OAUTH_BETA.to_string());
        }
        Arc::new(AnthropicModel {
            id: LanguageModelId::from(model.id.clone()),
            model,
            authentication: Authentication::Subscription(self.state.clone()),
            http_client: self.state.read(cx).http_client.clone(),
            request_limiter: RateLimiter::new(4),
        })
    }
}

impl LanguageModelProviderState for ClaudeSubscriptionProvider {
    type ObservableEntity = State;
    fn observable_entity(&self) -> Option<Entity<State>> {
        Some(self.state.clone())
    }
}

impl LanguageModelProvider for ClaudeSubscriptionProvider {
    fn id(&self) -> LanguageModelProviderId {
        PROVIDER_ID
    }
    fn name(&self) -> LanguageModelProviderName {
        PROVIDER_NAME
    }
    fn icon(&self) -> IconOrSvg {
        IconOrSvg::Icon(IconName::AiClaude)
    }
    fn default_model(&self, cx: &App) -> Option<Arc<dyn LanguageModel>> {
        pick_preferred_model(&self.state.read(cx).models, DEFAULT_MODEL_PREFIXES)
            .map(|model| self.create_model(model, cx))
    }
    fn default_fast_model(&self, cx: &App) -> Option<Arc<dyn LanguageModel>> {
        pick_preferred_model(&self.state.read(cx).models, DEFAULT_FAST_MODEL_PREFIXES)
            .map(|model| self.create_model(model, cx))
    }
    fn provided_models(&self, cx: &App) -> Vec<Arc<dyn LanguageModel>> {
        models_with_overrides(&self.state.read(cx).models, cx)
            .into_iter()
            .map(|model| self.create_model(model, cx))
            .collect()
    }
    fn is_authenticated(&self, cx: &App) -> bool {
        self.state.read(cx).credentials.is_some()
    }
    fn authenticate(&self, cx: &mut App) -> Task<Result<(), AuthenticateError>> {
        let state = self.state.clone();
        let load = state.read(cx).load_task.clone();
        cx.spawn(async move |cx| {
            if let Some(load) = load {
                load.await
                    .map_err(|error| AuthenticateError::Other(anyhow!("{error:#}")))?;
            }
            // A startup fetch that failed transiently (offline, 5xx) would otherwise leave the
            // provider signed in with no models until the next sign-in.
            let models = state.update(cx, |state, cx| {
                if state.credentials.is_none() {
                    return Err(AuthenticateError::CredentialsNotFound);
                }
                Ok(state.models.is_empty().then(|| state.fetch_models(cx)))
            })?;
            if let Some(models) = models {
                models.await.map_err(AuthenticateError::Other)?;
            }
            Ok(())
        })
    }
    fn settings_view(&self, _cx: &mut App) -> Option<ProviderSettingsView> {
        let state = self.state.clone();
        Some(ProviderSettingsView::Inline(InlineProviderSettings {
            title: Some("Claude Subscription".into()),
            description: Some(InlineDescription::Text("Sign in with your Claude account. Usage is subject to your plan's third-party usage credits and limits.".into())),
            create_view: Arc::new(move |_window, cx| {
                cx.new(|cx| {
                    cx.observe(&state, |_, _, cx| cx.notify()).detach();
                    ConfigurationView { state: state.clone() }
                }).into()
            }),
        }))
    }
    fn fast_mode_confirmation(&self, _cx: &App) -> Option<FastModeConfirmation> {
        Some(claude_fast_mode_confirmation())
    }
    fn authentication_error_message(&self) -> SharedString {
        "Your Claude session is invalid or expired. Sign in again in Settings > AI > LLM Providers > Claude Subscription.".into()
    }
    fn missing_credentials_error_message(&self) -> SharedString {
        "Sign in with Claude in Settings > AI > LLM Providers > Claude Subscription.".into()
    }
}

struct ConfigurationView {
    state: Entity<State>,
}

impl Render for ConfigurationView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let busy = state.sign_in_task.is_some() || state.load_task.is_some() || state.persisting;
        let error = state.error.clone();
        let mut view = v_flex().gap_2();
        if let Some(credentials) = &state.credentials {
            let label = credentials
                .email
                .as_ref()
                .map(|email| format!("Signed in as {email}"))
                .unwrap_or_else(|| "Signed in to Claude".to_string());
            let state = self.state.clone();
            view = view.child(
                ConfiguredApiCard::new("claude-sign-out", SharedString::from(label))
                    .button_label("Sign Out")
                    .on_click(move |_, _, cx| {
                        state
                            .update(cx, |state, cx| state.sign_out(cx))
                            .detach_and_log_err(cx);
                    }),
            );
        } else {
            let state_entity = self.state.clone();
            view = view.child(
                Button::new(
                    "claude-sign-in",
                    if state.sign_in_task.is_some() {
                        "Signing in…"
                    } else {
                        "Sign In with Claude"
                    },
                )
                .style(ButtonStyle::Outlined)
                .disabled(busy)
                .loading(busy)
                .on_click(move |_, _, cx| {
                    state_entity.update(cx, |state, cx| state.sign_in(cx));
                }),
            );
            if state.sign_in_task.is_some() && !state.persisting {
                let state = self.state.clone();
                view = view.child(Button::new("claude-cancel-sign-in", "Cancel").on_click(
                    move |_, _, cx| {
                        state.update(cx, |state, cx| state.cancel_sign_in(cx));
                    },
                ));
            }
        }
        view.when_some(error, |view, error| {
            view.child(Label::new(error).color(Color::Error))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{StreamExt as _, channel::oneshot};
    use gpui::{AsyncApp, TestAppContext};
    use http_client::{FakeHttpClient, Response};
    use language_model::{
        LanguageModelCompletionEvent, LanguageModelRequest, LanguageModelRequestMessage,
        MessageContent,
    };
    use parking_lot::Mutex as ParkingMutex;
    use serde_json::json;
    use settings::SettingsStore;
    use std::{
        future::Future,
        pin::Pin,
        sync::atomic::{AtomicUsize, Ordering},
    };

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            release_channel::init("1.2.3".parse().expect("test version"), cx);
        });
    }

    struct TestCredentials {
        url: &'static str,
        stored: ParkingMutex<Option<Vec<u8>>>,
        read_gate: ParkingMutex<Option<oneshot::Receiver<()>>>,
        write_gate: ParkingMutex<Option<oneshot::Receiver<()>>>,
    }

    impl Default for TestCredentials {
        fn default() -> Self {
            Self {
                url: CREDENTIALS_KEY,
                stored: Default::default(),
                read_gate: Default::default(),
                write_gate: Default::default(),
            }
        }
    }

    impl CredentialsProvider for TestCredentials {
        fn read_credentials<'a>(
            &'a self,
            url: &'a str,
            _cx: &'a AsyncApp,
        ) -> Pin<Box<dyn Future<Output = Result<Option<(String, Vec<u8>)>>> + 'a>> {
            assert_eq!(url, self.url);
            Box::pin(async move {
                let stored = self.stored.lock().clone();
                let gate = self.read_gate.lock().take();
                if let Some(gate) = gate {
                    gate.await?;
                }
                Ok(stored.map(|bytes| ("Bearer".to_string(), bytes)))
            })
        }
        fn write_credentials<'a>(
            &'a self,
            url: &'a str,
            username: &'a str,
            password: &'a [u8],
            _cx: &'a AsyncApp,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
            assert_eq!(url, self.url);
            assert_eq!(username, "Bearer");
            Box::pin(async move {
                let gate = self.write_gate.lock().take();
                if let Some(gate) = gate {
                    gate.await?;
                }
                *self.stored.lock() = Some(password.to_vec());
                Ok(())
            })
        }
        fn delete_credentials<'a>(
            &'a self,
            url: &'a str,
            _cx: &'a AsyncApp,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
            assert_eq!(url, self.url);
            Box::pin(async move {
                *self.stored.lock() = None;
                Ok(())
            })
        }
    }

    fn stored_credentials(expires_at: u64) -> Arc<TestCredentials> {
        Arc::new(TestCredentials {
            stored: ParkingMutex::new(Some(
                serde_json::to_vec(&Credentials {
                    access_token: "test-access".into(),
                    refresh_token: "test-refresh".into(),
                    expires_at,
                    email: Some("test@example.com".into()),
                })
                .expect("valid credential fixture"),
            )),
            ..Default::default()
        })
    }

    fn models_response() -> String {
        json!({"data": [{"id": "claude-haiku-4-5", "display_name": "Claude Haiku 4.5", "max_input_tokens": 200000, "max_tokens": 32000}], "has_more": false}).to_string()
    }

    #[gpui::test]
    async fn api_key_and_subscription_share_request_pipeline(cx: &mut TestAppContext) {
        init_test(cx);
        use language_model::{LanguageModelCompletionError, LanguageModelRequestTool};
        use std::sync::atomic::AtomicBool;

        const API_URL: &str = "https://anthropic.example.test";
        cx.update(|cx| {
            cx.update_global::<SettingsStore, _>(|settings, cx| {
                settings
                    .set_user_settings(
                        &json!({"language_models": {"anthropic": {
                                "api_url": API_URL,
                                "custom_headers": {"x-route": "same-route"},
                        "available_models": [{
                            "name": "claude-fable-5-1", "display_name": "Configured Fable",
                            "max_tokens": 200000, "max_output_tokens": 4096,
                            "mode": {"type": "adaptive"}
                        }]
                            }}})
                        .to_string(),
                        cx,
                    )
                    .expect("valid settings");
            });
        });
        let requests = Arc::new(ParkingMutex::new(Vec::new()));
        let reject = Arc::new(AtomicBool::new(false));
        let http_client = FakeHttpClient::create({
            let requests = requests.clone();
            let reject = reject.clone();
            move |mut request| {
                let requests = requests.clone();
                let reject = reject.clone();
                async move {
                    assert_eq!(request.uri().host(), Some("anthropic.example.test"));
                    assert_eq!(request.headers()["x-route"], "same-route");
                    assert_eq!(
                        request.headers().get_all("authorization").iter().count(),
                        usize::from(!request.headers().contains_key("x-api-key"))
                    );
                    let headers = request.headers().clone();
                    let body = match request.uri().path() {
                        "/v1/models" => json!({"data": [{
                            "id": "claude-fable-5-1", "display_name": "Claude Fable 5.1",
                            "max_input_tokens": 200000, "max_tokens": 32000
                        }], "has_more": false})
                        .to_string(),
                        "/v1/messages" => {
                            let mut body = String::new();
                            request.body_mut().read_to_string(&mut body).await?;
                            requests.lock().push((headers, body));
                            if reject.load(Ordering::SeqCst) {
                                return Ok(Response::builder().status(429).body(AsyncBody::from(
                                    r#"{"type":"error","error":{"type":"rate_limit_error","message":"Too many requests"}}"#
                                ))?);
                            }
                            [
                                json!({"type":"message_start","message":{"id":"message","type":"message","role":"assistant","model":"claude-fable-5-1","content":[],"usage":{"input_tokens":10,"output_tokens":0}}}),
                                json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool","name":"read_file","input":{}}}),
                                json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"demo.rs\"}"}}),
                                json!({"type":"content_block_stop","index":0}),
                                json!({"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":5}}),
                                json!({"type":"message_stop"}),
                            ].into_iter().map(|event| format!("data: {event}\n\n")).collect()
                        }
                        path => panic!("unexpected request: {path}"),
                    };
                    Ok(Response::builder()
                        .status(200)
                        .body(AsyncBody::from(body))?)
                }
            }
        });
        let api_storage = Arc::new(TestCredentials {
            url: API_URL,
            stored: ParkingMutex::new(Some(b"test-api-key".to_vec())),
            ..Default::default()
        });
        let api_provider = cx
            .update(|cx| AnthropicLanguageModelProvider::new(http_client.clone(), api_storage, cx));
        cx.update(|cx| api_provider.authenticate(cx))
            .await
            .expect("API key login");
        cx.run_until_parked();
        let oauth_provider = cx.update(|cx| {
            ClaudeSubscriptionProvider::new(http_client, stored_credentials(u64::MAX), cx)
        });
        cx.update(|cx| oauth_provider.authenticate(cx))
            .await
            .expect("OAuth login");
        let models = [
            cx.read(|cx| api_provider.provided_models(cx))
                .into_iter()
                .find(|model| model.id().0.as_ref() == "claude-fable-5-1")
                .expect("API model"),
            cx.read(|cx| oauth_provider.provided_models(cx))
                .into_iter()
                .find(|model| model.id().0.as_ref() == "claude-fable-5-1")
                .expect("OAuth model"),
        ];
        for system_cache in [false, true] {
            requests.lock().clear();
            reject.store(false, Ordering::SeqCst);
            let request = LanguageModelRequest {
                messages: vec![
                    LanguageModelRequestMessage {
                        role: language_model::Role::System,
                        content: vec![MessageContent::Text(
                            "Native Zed system instructions".into(),
                        )],
                        cache: system_cache,
                        reasoning_details: None,
                    },
                    LanguageModelRequestMessage {
                        role: language_model::Role::User,
                        content: vec![MessageContent::Text("Read demo.rs".into())],
                        cache: false,
                        reasoning_details: None,
                    },
                ],
                tools: vec![LanguageModelRequestTool::function(
                    "read_file".into(),
                    "Read a file".into(),
                    json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
                    false,
                )],
                ..Default::default()
            };
            let mut results = Vec::new();
            for model in &models {
                results.push(
                    model
                        .stream_completion(request.clone(), &cx.to_async())
                        .await
                        .expect("completion")
                        .collect::<Vec<_>>()
                        .await
                        .into_iter()
                        .collect::<Result<Vec<_>, _>>()
                        .expect("valid events"),
                );
            }
            assert_eq!(results[0], results[1]);
            assert!(results[0].iter().any(|event| matches!(event, LanguageModelCompletionEvent::ToolUse(tool) if tool.is_input_complete && tool.raw_input == "{\"path\":\"demo.rs\"}")));
            {
                let requests = requests.lock();
                assert_eq!(requests.len(), 2);
                let mut body: serde_json::Value =
                    serde_json::from_str(&requests[0].1).expect("request JSON");
                let mut oauth_body: serde_json::Value =
                    serde_json::from_str(&requests[1].1).expect("OAuth request JSON");
                let oauth_system = oauth_body["system"]
                    .as_array_mut()
                    .expect("OAuth system blocks");
                assert_eq!(
                    oauth_system.remove(0),
                    json!({
                        "type": "text",
                        "text": "x-anthropic-billing-header: cc_version=zed/1.2.3; cc_entrypoint=zed;"
                    })
                );
                if let Some(system) = body["system"].as_str() {
                    body["system"] = json!([{"type":"text", "text":system}]);
                }
                assert_eq!(body, oauth_body);
                assert_eq!(body["max_tokens"], 4096);
                assert_eq!(body["model"], "claude-fable-5-1");
                assert_eq!(requests[0].0["x-api-key"], "test-api-key");
                assert!(!requests[0].0.contains_key("authorization"));
                assert!(!requests[1].0.contains_key("x-api-key"));
                assert_eq!(requests[1].0["authorization"], "Bearer test-access");
                assert!(requests[1].0["authorization"].is_sensitive());
                let mut api_headers = requests[0].0.clone();
                let mut oauth_headers = requests[1].0.clone();
                api_headers.remove("x-api-key");
                oauth_headers.remove("authorization");
                oauth_headers.insert("anthropic-beta", api_headers["anthropic-beta"].clone());
                assert_eq!(api_headers, oauth_headers);
                assert_eq!(
                    requests[1].0["anthropic-beta"]
                        .to_str()
                        .expect("beta header"),
                    format!(
                        "{},{}",
                        requests[0].0["anthropic-beta"]
                            .to_str()
                            .expect("beta header"),
                        OAUTH_BETA
                    )
                );
            }
            reject.store(true, Ordering::SeqCst);
            for model in &models {
                let error = match model
                    .stream_completion(request.clone(), &cx.to_async())
                    .await
                {
                    Ok(_) => panic!("request should be rejected"),
                    Err(error) => error,
                };
                match error {
                    LanguageModelCompletionError::ProviderRejection {
                        provider,
                        status,
                        category,
                        message,
                        ..
                    } => {
                        assert_eq!(provider, model.provider_name());
                        assert_eq!(status.map(|status| status.as_u16()), Some(429));
                        assert_eq!(category, language_model::ProviderErrorCategory::RateLimit);
                        assert_eq!(message, "Too many requests");
                    }
                    error => panic!("unexpected error: {error:?}"),
                }
            }
        }
    }

    #[gpui::test]
    async fn restores_login_and_streams_with_subscription_headers(cx: &mut TestAppContext) {
        init_test(cx);
        let requests = Arc::new(AtomicUsize::new(0));
        let http_client = FakeHttpClient::create({
            let requests = requests.clone();
            move |mut request| {
                let requests = requests.clone();
                async move {
                    assert_eq!(request.headers()["authorization"], "Bearer test-access");
                    assert!(!request.headers().contains_key("x-api-key"));
                    assert!(
                        request.headers()["anthropic-beta"]
                            .to_str()?
                            .contains(OAUTH_BETA)
                    );
                    let body = match request.uri().path() {
                        "/v1/models" => models_response(),
                        "/v1/messages" => {
                            requests.fetch_add(1, Ordering::SeqCst);
                            let mut body = String::new();
                            request.body_mut().read_to_string(&mut body).await?;
                            let body: serde_json::Value = serde_json::from_str(&body)?;
                            assert_eq!(body["model"], "claude-haiku-4-5");
                            assert_eq!(
                                body["system"],
                                json!([{
                                    "type": "text",
                                    "text": "x-anthropic-billing-header: cc_version=zed/1.2.3; cc_entrypoint=zed;"
                                }])
                            );
                            assert_eq!(
                                body["messages"][0]["content"][0]["text"],
                                "Reply ZED_CLAUDE_OK"
                            );
                            assert_eq!(body["stream"], true);
                            [
                                json!({"type":"message_start", "message":{"id":"test-message","type":"message","role":"assistant","model":"claude-haiku-4-5","content":[],"usage":{"input_tokens":10,"output_tokens":0}}}),
                                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ZED_CLAUDE_OK"}}),
                                json!({"type":"content_block_stop","index":0}),
                                json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}),
                                json!({"type":"message_stop"}),
                            ].into_iter().map(|event| format!("data: {event}\n\n")).collect()
                        }
                        path => panic!("unexpected request: {path}"),
                    };
                    Ok(Response::builder()
                        .status(200)
                        .body(AsyncBody::from(body))?)
                }
            }
        });
        let storage = stored_credentials(u64::MAX);
        let provider = cx.update(|cx| ClaudeSubscriptionProvider::new(http_client, storage, cx));
        cx.update(|cx| provider.authenticate(cx))
            .await
            .expect("restored login");
        let model = cx
            .read(|cx| provider.default_model(cx))
            .expect("discovered model");
        assert_eq!(model.provider_id(), PROVIDER_ID);
        assert!(cx.read(|cx| model.api_key(cx)).is_none());
        let request = LanguageModelRequest {
            messages: vec![LanguageModelRequestMessage {
                role: language_model::Role::User,
                content: vec![MessageContent::Text("Reply ZED_CLAUDE_OK".into())],
                cache: false,
                reasoning_details: None,
            }],
            ..Default::default()
        };
        let mut stream = model
            .stream_completion(request, &cx.to_async())
            .await
            .expect("stream");
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            if let LanguageModelCompletionEvent::Text(chunk) = event.expect("valid event") {
                text.push_str(&chunk);
            }
        }
        assert_eq!(text, "ZED_CLAUDE_OK");
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[gpui::test]
    async fn concurrent_refresh_saves_rotated_credentials_once(cx: &mut TestAppContext) {
        init_test(cx);
        let (send, receive) = oneshot::channel();
        let gate = Arc::new(ParkingMutex::new(Some(receive)));
        let refreshes = Arc::new(AtomicUsize::new(0));
        let http_client = FakeHttpClient::create({
            let refreshes = refreshes.clone();
            move |mut request| {
                let gate = gate.clone();
                let refreshes = refreshes.clone();
                async move {
                    let body = if request.uri().path() == "/v1/oauth/token" {
                        refreshes.fetch_add(1, Ordering::SeqCst);
                        let mut body = String::new();
                        request.body_mut().read_to_string(&mut body).await?;
                        let body: serde_json::Value = serde_json::from_str(&body)?;
                        assert_eq!(body["refresh_token"], "test-refresh");
                        let receive = gate.lock().take().expect("one refresh");
                        receive.await?;
                        json!({"access_token":"rotated-access", "refresh_token":"rotated-refresh", "expires_in":3600}).to_string()
                    } else {
                        assert_eq!(request.uri().path(), "/v1/models");
                        assert_eq!(request.headers()["authorization"], "Bearer rotated-access");
                        models_response()
                    };
                    Ok(Response::builder()
                        .status(200)
                        .body(AsyncBody::from(body))?)
                }
            }
        });
        let storage = stored_credentials(0);
        let provider =
            cx.update(|cx| ClaudeSubscriptionProvider::new(http_client, storage.clone(), cx));
        let load = cx.update(|cx| provider.authenticate(cx));
        cx.run_until_parked();
        let first = provider
            .state
            .update(cx, |state, cx| state.fresh_credentials(cx));
        let second = provider
            .state
            .update(cx, |state, cx| state.fresh_credentials(cx));
        send.send(()).expect("pending refresh");
        load.await.expect("authenticated");
        assert_eq!(
            first.await.expect("first request").access_token,
            "rotated-access"
        );
        assert_eq!(
            second.await.expect("second request").access_token,
            "rotated-access"
        );
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        let stored: Credentials =
            serde_json::from_slice(storage.stored.lock().as_ref().expect("saved"))
                .expect("valid JSON");
        assert_eq!(stored.refresh_token, "rotated-refresh");
        assert_eq!(stored.email.as_deref(), Some("test@example.com"));
    }

    #[gpui::test]
    async fn refresh_errors_preserve_only_retryable_sessions(cx: &mut TestAppContext) {
        init_test(cx);
        for status in [400, 401, 403, 429, 500] {
            let http_client = FakeHttpClient::create(move |request| async move {
                assert_eq!(request.uri().path(), "/v1/oauth/token");
                Ok(Response::builder()
                    .status(status)
                    .body(AsyncBody::from("{}"))?)
            });
            let storage = stored_credentials(0);
            let provider =
                cx.update(|cx| ClaudeSubscriptionProvider::new(http_client, storage.clone(), cx));
            assert!(cx.update(|cx| provider.authenticate(cx)).await.is_err());
            let retryable = matches!(status, 429 | 500);
            assert_eq!(
                cx.read(|cx| provider.is_authenticated(cx)),
                retryable,
                "HTTP {status}"
            );
            assert_eq!(storage.stored.lock().is_some(), retryable, "HTTP {status}");
        }
    }

    #[gpui::test]
    async fn rejected_refresh_surfaces_authentication_error(cx: &mut TestAppContext) {
        init_test(cx);
        let http_client = FakeHttpClient::create(|request| async move {
            let (status, body) = match request.uri().path() {
                "/v1/models" => (200, models_response()),
                "/v1/oauth/token" => (401, "{}".to_string()),
                path => panic!("unexpected request: {path}"),
            };
            Ok(Response::builder()
                .status(status)
                .body(AsyncBody::from(body))?)
        });
        let storage = stored_credentials(u64::MAX);
        let provider =
            cx.update(|cx| ClaudeSubscriptionProvider::new(http_client, storage.clone(), cx));
        cx.update(|cx| provider.authenticate(cx))
            .await
            .expect("restored login");
        let model = cx
            .read(|cx| provider.default_model(cx))
            .expect("discovered model");
        provider.state.update(cx, |state, _| {
            state.credentials.as_mut().expect("signed in").expires_at = 0;
        });
        let error = match model
            .stream_completion(LanguageModelRequest::default(), &cx.to_async())
            .await
        {
            Ok(_) => panic!("expired session must be rejected"),
            Err(error) => error,
        };
        match error {
            language_model::LanguageModelCompletionError::ProviderRejection {
                provider,
                status,
                category,
                ..
            } => {
                assert_eq!(provider, PROVIDER_NAME);
                assert_eq!(status.map(|status| status.as_u16()), Some(401));
                assert_eq!(
                    category,
                    language_model::ProviderErrorCategory::Authentication
                );
            }
            error => panic!("unexpected error: {error:?}"),
        }
        assert!(!cx.read(|cx| provider.is_authenticated(cx)));
        assert!(storage.stored.lock().is_none());
    }

    #[gpui::test]
    async fn authenticate_refetches_models_after_transient_failure(cx: &mut TestAppContext) {
        init_test(cx);
        let attempts = Arc::new(AtomicUsize::new(0));
        let http_client = FakeHttpClient::create({
            let attempts = attempts.clone();
            move |request| {
                let attempts = attempts.clone();
                async move {
                    assert_eq!(request.uri().path(), "/v1/models");
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Ok(Response::builder()
                            .status(500)
                            .body(AsyncBody::from("{}"))?);
                    }
                    Ok(Response::builder()
                        .status(200)
                        .body(AsyncBody::from(models_response()))?)
                }
            }
        });
        let provider = cx.update(|cx| {
            ClaudeSubscriptionProvider::new(http_client, stored_credentials(u64::MAX), cx)
        });
        assert!(cx.update(|cx| provider.authenticate(cx)).await.is_err());
        assert!(cx.read(|cx| provider.is_authenticated(cx)));
        assert!(
            provider
                .state
                .read_with(cx, |state, _| state.models.is_empty())
        );

        cx.update(|cx| provider.authenticate(cx))
            .await
            .expect("second authenticate refetches models");
        assert_eq!(
            provider.state.read_with(cx, |state, _| state.models.len()),
            1
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[gpui::test]
    async fn sign_out_during_keychain_write_removes_rotated_credentials(cx: &mut TestAppContext) {
        init_test(cx);
        let http_client = FakeHttpClient::create(|request| async move {
            assert_eq!(request.uri().path(), "/v1/oauth/token");
            Ok(Response::builder().status(200).body(AsyncBody::from(json!({
                "access_token":"rotated-access", "refresh_token":"rotated-refresh", "expires_in":3600,
            }).to_string()))?)
        });
        let storage = stored_credentials(0);
        let (send, receive) = oneshot::channel();
        *storage.write_gate.lock() = Some(receive);
        let provider =
            cx.update(|cx| ClaudeSubscriptionProvider::new(http_client, storage.clone(), cx));
        let load = cx.update(|cx| provider.authenticate(cx));
        cx.run_until_parked();
        assert!(
            storage.write_gate.lock().is_none(),
            "refresh reached keychain write"
        );
        let sign_out = provider.state.update(cx, |state, cx| state.sign_out(cx));
        assert!(!cx.read(|cx| provider.is_authenticated(cx)));
        send.send(()).expect("pending write");
        sign_out.await.expect("sign out");
        assert!(load.await.is_err());
        assert!(!cx.read(|cx| provider.is_authenticated(cx)));
        assert!(
            storage.stored.lock().is_none(),
            "no credentials restored on restart"
        );
    }

    #[gpui::test]
    async fn sign_out_invalidates_pending_initial_load(cx: &mut TestAppContext) {
        init_test(cx);
        let http_client = FakeHttpClient::create(|_| async {
            panic!("signed-out credentials must never reach the network")
        });
        let storage = stored_credentials(u64::MAX);
        let (send, receive) = oneshot::channel();
        *storage.read_gate.lock() = Some(receive);
        let provider =
            cx.update(|cx| ClaudeSubscriptionProvider::new(http_client, storage.clone(), cx));
        let load = cx.update(|cx| provider.authenticate(cx));
        cx.run_until_parked();
        let sign_out = provider.state.update(cx, |state, cx| state.sign_out(cx));
        send.send(()).expect("pending load");
        sign_out.await.expect("sign out");
        assert!(load.await.is_err());
        assert!(!cx.read(|cx| provider.is_authenticated(cx)));
        assert!(storage.stored.lock().is_none());
    }

    #[gpui::test]
    async fn oauth_validates_state_and_exchanges_pkce_code(cx: &mut TestAppContext) {
        init_test(cx);
        use std::io::{Read as _, Write as _};
        // The loopback callback server wakes futures from an OS thread.
        cx.executor().allow_parking();
        let expected_parameters = Arc::new(ParkingMutex::new(
            None::<std::collections::HashMap<String, String>>,
        ));
        let exchanges = Arc::new(AtomicUsize::new(0));
        let http_client = FakeHttpClient::create({
            let expected_parameters = expected_parameters.clone();
            let exchanges = exchanges.clone();
            move |mut request| {
                let expected_parameters = expected_parameters.clone();
                let exchanges = exchanges.clone();
                async move {
                    let body = if request.uri().path() == "/v1/oauth/token" {
                        exchanges.fetch_add(1, Ordering::SeqCst);
                        let mut body = String::new();
                        request.body_mut().read_to_string(&mut body).await?;
                        let body: serde_json::Value = serde_json::from_str(&body)?;
                        let expected = expected_parameters
                            .lock()
                            .clone()
                            .expect("opened authorize URL");
                        assert_eq!(body["code"], "test-authorization-code");
                        assert_eq!(body["state"], expected["state"]);
                        assert_eq!(body["redirect_uri"], expected["redirect_uri"]);
                        assert_eq!(body["client_id"], CLIENT_ID);
                        let verifier = body["code_verifier"].as_str().expect("PKCE verifier");
                        assert_eq!(
                            URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
                            expected["code_challenge"]
                        );
                        json!({"access_token":"oauth-access", "refresh_token":"oauth-refresh", "expires_in":3600,"account":{"email_address":"test@example.com"}}).to_string()
                    } else {
                        assert_eq!(request.uri().path(), "/v1/models");
                        models_response()
                    };
                    Ok(Response::builder()
                        .status(200)
                        .body(AsyncBody::from(body))?)
                }
            }
        });
        let storage = Arc::new(TestCredentials::default());
        let provider =
            cx.update(|cx| ClaudeSubscriptionProvider::new(http_client, storage.clone(), cx));
        assert!(cx.update(|cx| provider.authenticate(cx)).await.is_err());
        for valid_state in [false, true] {
            provider.state.update(cx, |state, cx| state.sign_in(cx));
            cx.run_until_parked();
            let url =
                url::Url::parse(&cx.opened_url().expect("browser opened")).expect("valid URL");
            assert_eq!(url.origin().ascii_serialization(), "https://claude.com");
            let parameters = url
                .query_pairs()
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect::<std::collections::HashMap<_, _>>();
            assert_eq!(parameters["scope"], SCOPES);
            assert_eq!(parameters["code_challenge_method"], "S256");
            let mut redirect =
                url::Url::parse(&parameters["redirect_uri"]).expect("valid redirect");
            redirect
                .query_pairs_mut()
                .append_pair("code", "test-authorization-code")
                .append_pair(
                    "state",
                    if valid_state {
                        &parameters["state"]
                    } else {
                        "wrong-state"
                    },
                );
            *expected_parameters.lock() = Some(parameters);
            let task = provider.state.update(cx, |state, _| {
                state.sign_in_task.take().expect("sign-in task")
            });
            let mut socket = std::net::TcpStream::connect((
                redirect.host_str().expect("host"),
                redirect.port().expect("port"),
            ))
            .expect("listener");
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("timeout");
            write!(
                socket,
                "GET {}?{} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                redirect.path(),
                redirect.query().expect("query")
            )
            .expect("callback");
            let mut response = String::new();
            socket
                .read_to_string(&mut response)
                .expect("callback response");
            task.await;
            assert_eq!(cx.read(|cx| provider.is_authenticated(cx)), valid_state);
            assert_eq!(storage.stored.lock().is_some(), valid_state);
        }
        assert_eq!(
            exchanges.load(Ordering::SeqCst),
            1,
            "invalid state must never be exchanged"
        );
    }
}
