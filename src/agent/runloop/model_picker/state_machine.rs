use super::selection::supports_gpt5_none_reasoning;
use super::*;
use crate::agent::runloop::unified::external_url_guard::{
    ExternalUrlGuardContext, ExternalUrlOpenOutcome, request_external_url_open,
};
use crate::cli::auth::{complete_openai_login_with_tui_cancel, is_oauth_flow_cancelled, prepare_openai_login};

impl ModelPickerState {
    pub(super) fn handle_reasoning(&mut self, renderer: &mut AnsiRenderer, input: &str) -> Result<ModelPickerProgress> {
        if self.selection.is_none() {
            return Err(anyhow!("Reasoning requested before selecting a model"));
        }

        let normalized = input.to_ascii_lowercase();
        if matches!(normalized.as_str(), "off" | "disable") {
            return self.apply_reasoning_off_choice(renderer);
        }

        let level = match normalized.as_str() {
            "none" => Some(ReasoningEffortLevel::None),
            "easy" | "low" => Some(ReasoningEffortLevel::Low),
            "medium" => Some(ReasoningEffortLevel::Medium),
            "xhigh" => Some(ReasoningEffortLevel::XHigh),
            "max" => Some(ReasoningEffortLevel::Max),
            "hard" | "high" => Some(ReasoningEffortLevel::High),
            "skip" => Some(self.settings.current_reasoning),
            _ => None,
        };

        let Some(selected) = level else {
            renderer.line(
                MessageStyle::Error,
                "Unknown reasoning option. Use none, low, medium, high, xhigh, max, skip, or off.",
            )?;
            if let Some(progress) = self.prompt_reasoning_step(renderer)? {
                return Ok(progress);
            }
            return Ok(ModelPickerProgress::InProgress);
        };

        self.apply_reasoning_choice(renderer, selected)
    }

    fn prompt_reasoning_step(&mut self, renderer: &mut AnsiRenderer) -> Result<Option<ModelPickerProgress>> {
        let Some(selection) = self.selection.as_ref() else {
            return Err(anyhow!("Reasoning requested before selecting a model"));
        };
        if self.settings.inline_enabled {
            render_reasoning_inline(renderer, selection, self.settings.current_reasoning)?;
            return Ok(None);
        }

        match select_reasoning_with_ratatui(selection, self.settings.current_reasoning) {
            Ok(Some(ReasoningChoice::Level(level))) => self.apply_reasoning_choice(renderer, level).map(Some),
            Ok(Some(ReasoningChoice::Disable)) => self.apply_reasoning_off_choice(renderer).map(Some),
            Ok(None) => {
                prompt_reasoning_plain(renderer, selection, self.settings.current_reasoning)?;
                Ok(None)
            }
            Err(err) => {
                if err.is::<SelectionInterrupted>() {
                    return Err(err);
                }
                renderer.line(
                    MessageStyle::Info,
                    &format!("Interactive reasoning selector unavailable ({err}). Falling back to manual input."),
                )?;
                prompt_reasoning_plain(renderer, selection, self.settings.current_reasoning)?;
                Ok(None)
            }
        }
    }

    fn prompt_api_key_step(&mut self, renderer: &mut AnsiRenderer) -> Result<()> {
        let Some(selection) = self.selection.as_ref() else {
            return Err(anyhow!("API key requested before selecting a model"));
        };
        if self.settings.inline_enabled {
            show_secure_api_modal(renderer, selection, self.settings.workspace.as_deref());
            return Ok(());
        }
        prompt_api_key_plain(renderer, selection, self.settings.workspace.as_deref())
    }

    fn prompt_service_tier_step(&mut self, renderer: &mut AnsiRenderer) -> Result<Option<ModelPickerProgress>> {
        let Some(selection) = self.selection.as_ref() else {
            return Err(anyhow!("Service tier requested before selecting a model"));
        };
        if self.settings.inline_enabled {
            render_service_tier_inline(renderer, selection, self.settings.current_service_tier)?;
            return Ok(None);
        }

        match select_service_tier_with_ratatui(selection, self.settings.current_service_tier) {
            Ok(Some(ServiceTierChoice::ProjectDefault)) => self.apply_service_tier_choice(renderer, None).map(Some),
            Ok(Some(ServiceTierChoice::Flex)) => self
                .apply_service_tier_choice(renderer, Some(OpenAIServiceTier::Flex))
                .map(Some),
            Ok(Some(ServiceTierChoice::Priority)) => self
                .apply_service_tier_choice(renderer, Some(OpenAIServiceTier::Priority))
                .map(Some),
            Ok(None) => {
                prompt_service_tier_plain(renderer, selection, self.settings.current_service_tier)?;
                Ok(None)
            }
            Err(err) => {
                if err.is::<SelectionInterrupted>() {
                    return Err(err);
                }
                renderer.line(
                    MessageStyle::Info,
                    &format!("Interactive service tier selector unavailable ({err}). Falling back to manual input."),
                )?;
                prompt_service_tier_plain(renderer, selection, self.settings.current_service_tier)?;
                Ok(None)
            }
        }
    }

    fn continue_after_reasoning(&mut self, renderer: &mut AnsiRenderer) -> Result<ModelPickerProgress> {
        // Insert MiMo auth method step after reasoning, before service tier
        if self
            .selection
            .as_ref()
            .map(|d| d.provider_enum == Some(Provider::MiMo))
            .unwrap_or(false)
            && self.selected_mimo_auth.is_none()
        {
            self.step = PickerStep::AwaitMiMoAuthMethod;
            if let Some(progress) = self.prompt_mimo_auth_method_step(renderer)? {
                return Ok(progress);
            }
            return Ok(ModelPickerProgress::InProgress);
        }

        if self
            .selection
            .as_ref()
            .map(|detail| detail.service_tier_supported)
            .unwrap_or(false)
        {
            self.step = PickerStep::AwaitServiceTier;
            if let Some(progress) = self.prompt_service_tier_step(renderer)? {
                return Ok(progress);
            }
            return Ok(ModelPickerProgress::InProgress);
        }

        self.finish_after_service_tier(renderer)
    }

    pub(super) fn handle_mimo_auth_method(
        &mut self,
        renderer: &mut AnsiRenderer,
        input: &str,
    ) -> Result<ModelPickerProgress> {
        let normalized = input.to_ascii_lowercase();
        let auth_method = match normalized.as_str() {
            "token-plan" | "token_plan" | "tokenplan" | "tp" | "2" => MiMoAuthMethod::TokenPlan,
            "payg" | "pay-as-you-go" | "pay_as_you_go" | "1" | "skip" => MiMoAuthMethod::PayAsYouGo,
            _ => {
                renderer.line(
                    MessageStyle::Error,
                    "Unknown auth method. Use 'token-plan' or 'pay-as-you-go', or type 'skip' for default.",
                )?;
                if self.settings.inline_enabled {
                    render_mimo_auth_method_inline(renderer)?;
                } else {
                    prompt_mimo_auth_method_plain(renderer)?;
                }
                return Ok(ModelPickerProgress::InProgress);
            }
        };

        self.selected_mimo_auth = Some(auth_method);

        let Some(current) = self.selection.clone() else {
            return Err(anyhow!("MiMo auth method requested before selecting a model"));
        };
        let mut selection = current;
        selection.mimo_auth_method = Some(auth_method);
        selection.env_key = auth_method.env_key().to_string();
        self.pending_api_key = None;
        self.pending_credential_source = None;
        if let Some(resolved) = self.resolve_selection_credential(&selection)? {
            let message = existing_credential_message(&selection, &resolved);
            self.apply_resolved_credential(&mut selection, resolved);
            renderer.line(MessageStyle::Info, &message)?;
        } else {
            selection.requires_api_key = true;
            selection.uses_chatgpt_auth = false;
        }
        self.selection = Some(selection);

        self.finish_after_mimo_auth_method(renderer)
    }

    pub(super) fn finish_after_mimo_auth_method(&mut self, renderer: &mut AnsiRenderer) -> Result<ModelPickerProgress> {
        if self
            .selection
            .as_ref()
            .map(|detail| detail.service_tier_supported)
            .unwrap_or(false)
        {
            self.step = PickerStep::AwaitServiceTier;
            if let Some(progress) = self.prompt_service_tier_step(renderer)? {
                return Ok(progress);
            }
            return Ok(ModelPickerProgress::InProgress);
        }

        self.finish_after_service_tier(renderer)
    }

    fn prompt_mimo_auth_method_step(&mut self, renderer: &mut AnsiRenderer) -> Result<Option<ModelPickerProgress>> {
        if self.settings.inline_enabled {
            render_mimo_auth_method_inline(renderer)?;
            return Ok(None);
        }

        prompt_mimo_auth_method_plain(renderer)?;
        Ok(None)
    }

    fn finish_after_service_tier(&mut self, renderer: &mut AnsiRenderer) -> Result<ModelPickerProgress> {
        if self.selection.as_ref().map(|detail| detail.requires_api_key).unwrap_or(false) {
            self.step = PickerStep::AwaitApiKey;
            self.prompt_api_key_step(renderer)?;
            return Ok(ModelPickerProgress::InProgress);
        }

        let result = self.build_result();
        Ok(ModelPickerProgress::Completed(result?))
    }

    pub(super) fn apply_reasoning_choice(
        &mut self,
        renderer: &mut AnsiRenderer,
        level: ReasoningEffortLevel,
    ) -> Result<ModelPickerProgress> {
        let Some(_selection) = self.selection.as_ref() else {
            return Err(anyhow!("Reasoning requested before selecting a model"));
        };
        self.selected_reasoning = Some(level);
        self.continue_after_reasoning(renderer)
    }

    pub(super) fn apply_reasoning_off_choice(&mut self, renderer: &mut AnsiRenderer) -> Result<ModelPickerProgress> {
        let Some(current_selection) = self.selection.as_ref() else {
            return Err(anyhow!("Reasoning requested before selecting a model"));
        };

        // For GPT-5.2 and GPT-5.3 Codex models, disable reasoning by setting effort to "none" on the same model
        // rather than switching to a different model
        if supports_gpt5_none_reasoning(&current_selection.model_id) {
            self.selected_reasoning = Some(ReasoningEffortLevel::None);
            renderer.line(
                MessageStyle::Info,
                &format!("Reasoning disabled for {} by setting effort to 'none'.", current_selection.model_display),
            )?;

            return self.continue_after_reasoning(renderer);
        }

        let Some(ref target_model) = current_selection.reasoning_off_model else {
            renderer.line(MessageStyle::Error, "This model does not have a non-reasoning variant.")?;
            if self.settings.inline_enabled {
                render_reasoning_inline(renderer, current_selection, self.settings.current_reasoning)?;
            } else {
                prompt_reasoning_plain(renderer, current_selection, self.settings.current_reasoning)?;
            }
            return Ok(ModelPickerProgress::InProgress);
        };

        let Some(option) = self
            .settings
            .options
            .iter()
            .find(|candidate| candidate.id.eq_ignore_ascii_case(&target_model.as_str()))
        else {
            renderer.line(
                MessageStyle::Error,
                &format!("Unable to locate the non-reasoning variant {}.", target_model.as_str()),
            )?;
            if self.settings.inline_enabled {
                render_reasoning_inline(renderer, current_selection, self.settings.current_reasoning)?;
            } else {
                prompt_reasoning_plain(renderer, current_selection, self.settings.current_reasoning)?;
            }
            return Ok(ModelPickerProgress::InProgress);
        };

        self.selected_reasoning = Some(ReasoningEffortLevel::None);
        let mut new_selection = selection::selection_from_option_with_mode(option, self.storage_mode());
        if new_selection.provider_label != current_selection.provider_label {
            new_selection.provider_label = current_selection.provider_label.clone();
        }
        let alt_display = new_selection.model_display.clone();
        let alt_id = new_selection.model_id.clone();

        let progress = self.process_model_selection(renderer, new_selection)?;
        renderer.line(MessageStyle::Info, &format!("Reasoning disabled by switching to {alt_display} ({alt_id})."))?;
        Ok(progress)
    }

    pub(super) fn build_result(&self) -> Result<ModelSelectionResult> {
        let selection = self.selection.as_ref().ok_or_else(|| anyhow!("Model selection missing"))?;
        // A model switch can land on a route that exposes structured reasoning
        // but no configurable effort. Never carry the previous route's effort
        // into that result; `None` is the explicit disable value for the new
        // route and keeps finalization from issuing an invalid request.
        let chosen_reasoning = if selection.reasoning_effort_supported {
            self.selected_reasoning.unwrap_or(self.settings.current_reasoning)
        } else {
            ReasoningEffortLevel::None
        };
        let reasoning_changed = chosen_reasoning != self.settings.current_reasoning;
        let chosen_service_tier = self.selected_service_tier.unwrap_or(self.settings.current_service_tier);
        let service_tier_changed = chosen_service_tier != self.settings.current_service_tier;

        Ok(ModelSelectionResult {
            provider: selection.provider_key.clone(),
            provider_label: selection.provider_label.clone(),
            provider_enum: selection.provider_enum,
            model: selection.model_id.clone(),
            model_display: selection.model_display.clone(),
            known_model: selection.known_model,
            context_window: selection.context_window,
            reasoning_supported: selection.reasoning_supported,
            reasoning: chosen_reasoning,
            reasoning_changed,
            service_tier_supported: selection.service_tier_supported,
            service_tier: chosen_service_tier,
            service_tier_changed,
            api_key: self.pending_api_key.clone(),
            credential_source: self.pending_credential_source,
            env_key: selection.env_key.clone(),
            requires_api_key: selection.requires_api_key,
            uses_chatgpt_auth: selection.uses_chatgpt_auth,
            mimo_auth_method: selection.mimo_auth_method.or(self.selected_mimo_auth),
        })
    }

    pub(super) fn process_model_selection(
        &mut self,
        renderer: &mut AnsiRenderer,
        selection: SelectionDetail,
    ) -> Result<ModelPickerProgress> {
        let message =
            format!("Selected {} ({}) from {}.", selection.model_display, selection.model_id, selection.provider_label);
        renderer.line(MessageStyle::Info, &message)?;

        if matches!(selection.provider_enum, Some(Provider::HuggingFace)) {
            renderer.line(
                MessageStyle::Info,
                "Hugging Face uses HF_TOKEN (from environment variables or secure storage) and honors HUGGINGFACE_BASE_URL (default: https://router.huggingface.co/v1).",
            )?;
            if selection.requires_api_key {
                renderer.line(
                    MessageStyle::Info,
                    "No HF_TOKEN detected; you'll be prompted to paste it and it will be saved to secure storage.",
                )?;
            }
        }

        // A pending key belongs to the provider/key identity, not merely the
        // provider. Changing a configured key name must invalidate it.
        let provider_changed = self
            .selection
            .as_ref()
            .map(|prev| prev.provider_key != selection.provider_key)
            .unwrap_or(true);
        let credential_target_changed = self
            .selection
            .as_ref()
            .map(|prev| prev.provider_key != selection.provider_key || prev.env_key != selection.env_key)
            .unwrap_or(true);
        if credential_target_changed {
            self.pending_api_key = None;
            self.pending_credential_source = None;
        }
        if provider_changed {
            self.selected_service_tier = None;
            self.selected_mimo_auth = None;
        }
        let mut selection = selection;
        if !selection.env_key.trim().is_empty()
            && let Some(resolved) = self.resolve_selection_credential(&selection)?
        {
            let message = existing_credential_message(&selection, &resolved);
            self.apply_resolved_credential(&mut selection, resolved);
            renderer.line(MessageStyle::Info, &message)?;
        }

        if selection.requires_api_key && self.pending_api_key.is_some() {
            selection.requires_api_key = false;
        }

        if selection.requires_api_key {
            self.pending_credential_source = None;
        }

        let reasoning_effort_supported = selection.reasoning_effort_supported;
        self.selection = Some(selection);
        if !reasoning_effort_supported {
            let inherited = self.selected_reasoning.unwrap_or(self.settings.current_reasoning);
            self.selected_reasoning = Some(ReasoningEffortLevel::None);
            if inherited != ReasoningEffortLevel::None {
                renderer.line(
                    MessageStyle::Info,
                    "The selected route does not expose configurable reasoning effort; reasoning was disabled.",
                )?;
            }
        }
        if self
            .selection
            .as_ref()
            .map(|detail| detail.reasoning_effort_supported)
            .unwrap_or(false)
        {
            self.step = PickerStep::AwaitReasoning;
            if let Some(progress) = self.prompt_reasoning_step(renderer)? {
                return Ok(progress);
            }
            return Ok(ModelPickerProgress::InProgress);
        }

        self.continue_after_reasoning(renderer)
    }

    pub(super) fn handle_service_tier(
        &mut self,
        renderer: &mut AnsiRenderer,
        input: &str,
    ) -> Result<ModelPickerProgress> {
        let Some(selection) = self.selection.as_ref() else {
            return Err(anyhow!("Service tier requested before selecting a model"));
        };

        match input.to_ascii_lowercase().as_str() {
            "flex" => self.apply_service_tier_choice(renderer, Some(OpenAIServiceTier::Flex)),
            "priority" => self.apply_service_tier_choice(renderer, Some(OpenAIServiceTier::Priority)),
            "default" | "project" | "inherit" => self.apply_service_tier_choice(renderer, None),
            "skip" => self.apply_service_tier_choice(renderer, self.settings.current_service_tier),
            _ => {
                renderer
                    .line(MessageStyle::Error, "Unknown service tier option. Use flex, priority, default, or skip.")?;
                prompt_service_tier_plain(renderer, selection, self.settings.current_service_tier)?;
                Ok(ModelPickerProgress::InProgress)
            }
        }
    }

    pub(super) fn apply_service_tier_choice(
        &mut self,
        renderer: &mut AnsiRenderer,
        service_tier: Option<OpenAIServiceTier>,
    ) -> Result<ModelPickerProgress> {
        if self.selection.is_none() {
            return Err(anyhow!("Service tier requested before selecting a model"));
        }

        self.selected_service_tier = Some(service_tier);
        self.finish_after_service_tier(renderer)
    }

    pub(super) async fn handle_api_key(
        &mut self,
        renderer: &mut AnsiRenderer,
        input: &str,
        url_guard: ExternalUrlGuardContext<'_>,
    ) -> Result<ModelPickerProgress> {
        let Some(selection) = self.selection.as_ref() else {
            return Err(anyhow!("API key requested before selecting a model"));
        };

        if input.eq_ignore_ascii_case("login") && matches!(selection.provider_enum, Some(Provider::OpenAI)) {
            return self.handle_openai_login(renderer, url_guard).await;
        }

        if input.eq_ignore_ascii_case("skip") {
            return self.handle_skip_api_key(renderer, selection.clone()).await;
        }

        self.pending_api_key = Some(input.trim().to_string());
        self.pending_credential_source = None;
        renderer.close_modal();

        let result = self.build_result();
        Ok(ModelPickerProgress::Completed(result?))
    }

    async fn handle_openai_login(
        &mut self,
        renderer: &mut AnsiRenderer,
        url_guard: ExternalUrlGuardContext<'_>,
    ) -> Result<ModelPickerProgress> {
        if let Some(ctrl_c_state) = self.settings.ctrl_c_state.as_ref() {
            if ctrl_c_state.is_exit_requested() {
                return Ok(ModelPickerProgress::Exit);
            }
            if ctrl_c_state.is_cancel_requested() {
                ctrl_c_state.mark_cancel_handled();
                renderer.line(MessageStyle::Info, "OpenAI ChatGPT authentication cancelled.")?;
                return Ok(ModelPickerProgress::InProgress);
            }
        }

        let prepared = prepare_openai_login(self.settings.vt_cfg.as_ref())?;
        let auth_url = prepared.auth_url.clone();
        match request_external_url_open(url_guard, &auth_url).await? {
            ExternalUrlOpenOutcome::Opened => {
                renderer.line(MessageStyle::Info, "Opening browser for OpenAI ChatGPT authentication...")?;
                renderer.hyperlink_line(MessageStyle::Response, &auth_url)?;
            }
            ExternalUrlOpenOutcome::OpenFailed(err) => {
                renderer.line(MessageStyle::Info, "Opening browser for OpenAI ChatGPT authentication...")?;
                renderer.hyperlink_line(MessageStyle::Response, &auth_url)?;
                renderer.line(MessageStyle::Error, &format!("Failed to open browser automatically: {err}"))?;
                renderer.line(MessageStyle::Info, "Please open the URL manually in your browser.")?;
            }
            ExternalUrlOpenOutcome::Cancelled => {
                renderer.line(MessageStyle::Info, "Cancelled opening authentication link.")?;
                return Ok(ModelPickerProgress::InProgress);
            }
            ExternalUrlOpenOutcome::Exit => {
                return Ok(ModelPickerProgress::Exit);
            }
            ExternalUrlOpenOutcome::Unsupported => {
                renderer.line(MessageStyle::Error, "Blocked unsupported authentication link target.")?;
                return Ok(ModelPickerProgress::InProgress);
            }
        }
        let started = crate::cli::auth::begin_openai_login(prepared).await?;
        let Some(ctrl_c_state) = self.settings.ctrl_c_state.as_ref() else {
            return Err(anyhow!("OAuth login requires Ctrl+C state"));
        };
        let Some(ctrl_c_notify) = self.settings.ctrl_c_notify.as_ref() else {
            return Err(anyhow!("OAuth login requires Ctrl+C notifications"));
        };
        match complete_openai_login_with_tui_cancel(started, ctrl_c_state, ctrl_c_notify).await {
            Ok(_) => {}
            Err(err) if is_oauth_flow_cancelled(&err) => {
                if ctrl_c_state.is_exit_requested() {
                    return Ok(ModelPickerProgress::Exit);
                }
                renderer.line(MessageStyle::Info, "OpenAI ChatGPT authentication cancelled.")?;
                return Ok(ModelPickerProgress::Cancelled);
            }
            Err(err) => return Err(err),
        }
        if self.settings.inline_enabled {
            renderer.close_modal();
        }
        renderer.line(MessageStyle::Info, "Using ChatGPT subscription for OpenAI.")?;
        self.pending_api_key = None;
        self.pending_credential_source = Some(vtcode_config::api_keys::CredentialSource::OAuth);
        if let Some(current) = self.selection.as_mut() {
            current.requires_api_key = false;
            current.uses_chatgpt_auth = true;
        }
        let result = self.build_result();
        Ok(ModelPickerProgress::Completed(result?))
    }

    async fn handle_skip_api_key(
        &mut self,
        renderer: &mut AnsiRenderer,
        selection: SelectionDetail,
    ) -> Result<ModelPickerProgress> {
        if self.settings.inline_enabled {
            renderer.close_modal();
        }
        let mut selection = selection;
        match self.resolve_selection_credential(&selection)? {
            Some(resolved) => {
                let message = existing_credential_message(&selection, &resolved);
                self.apply_resolved_credential(&mut selection, resolved);
                renderer.line(MessageStyle::Info, &message)?;
                self.selection = Some(selection);
                let result = self.build_result();
                Ok(ModelPickerProgress::Completed(result?))
            }
            None => {
                renderer.line(
                    MessageStyle::Error,
                    &format!(
                        "No API key found for {}. Run `/secret add {}` to store one in secure storage, then type 'skip' to continue.",
                        selection.provider_label, selection.provider_key
                    ),
                )?;
                prompt_api_key_plain(renderer, &selection, self.settings.workspace.as_deref())?;
                Ok(ModelPickerProgress::InProgress)
            }
        }
    }

    fn resolve_selection_credential(
        &self,
        selection: &SelectionDetail,
    ) -> Result<Option<vtcode_config::api_keys::ResolvedCredential>> {
        vtcode_config::api_keys::resolve_credential_with_mode(
            &selection.provider_key,
            &selection.env_key,
            self.settings.workspace.as_deref(),
            self.storage_mode(),
        )
    }

    fn apply_resolved_credential(
        &mut self,
        selection: &mut SelectionDetail,
        resolved: vtcode_config::api_keys::ResolvedCredential,
    ) {
        selection.env_key = resolved.identity.key_name().to_owned();
        selection.requires_api_key = false;
        selection.uses_chatgpt_auth = resolved.source == vtcode_config::api_keys::CredentialSource::OAuth
            && matches!(selection.provider_enum, Some(Provider::OpenAI))
            && resolved.secret.is_none();
        self.pending_api_key = resolved.secret;
        self.pending_credential_source = Some(resolved.source);
    }
}

fn existing_credential_message(
    selection: &SelectionDetail,
    resolved: &vtcode_config::api_keys::ResolvedCredential,
) -> String {
    match resolved.source {
        vtcode_config::api_keys::CredentialSource::OAuth => oauth_auth_message(selection),
        vtcode_config::api_keys::CredentialSource::Env => format!(
            "Using existing environment variable {} for {}.",
            resolved.env_var.as_deref().unwrap_or(&selection.env_key),
            selection.provider_label
        ),
        vtcode_config::api_keys::CredentialSource::Workspace => format!(
            "Loaded {} from the workspace environment for {}.",
            resolved.env_var.as_deref().unwrap_or(&selection.env_key),
            selection.provider_label
        ),
        vtcode_config::api_keys::CredentialSource::SecureStorage => {
            format!("Using stored API key for {}.", selection.provider_label)
        }
        vtcode_config::api_keys::CredentialSource::ManagedAuth => {
            format!("Using managed authentication for {}.", selection.provider_label)
        }
        vtcode_config::api_keys::CredentialSource::Local => {
            format!("Using local authentication for {}.", selection.provider_label)
        }
    }
}

fn oauth_auth_message(selection: &SelectionDetail) -> String {
    if matches!(selection.provider_enum, Some(Provider::OpenAI)) {
        "Using ChatGPT subscription for OpenAI.".to_string()
    } else if matches!(selection.provider_enum, Some(Provider::Copilot)) {
        "Using managed authentication via GitHub Copilot CLI.".to_string()
    } else {
        format!("Using OAuth authentication for {}", selection.provider_label)
    }
}
