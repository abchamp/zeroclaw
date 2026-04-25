use super::traits::{Tool, ToolResult};
use crate::agent::loop_::run_tool_call_loop;
use crate::agent::prompt::{PromptContext, SystemPromptBuilder};
use crate::config::{DelegateAgentConfig, DelegateToolConfig, OrchestrationConfig};
use crate::observability::noop::NoopObserver;
use crate::providers::{self, ChatMessage, Provider};
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use async_trait::async_trait;
use parking_lot::Mutex;
use parking_lot::RwLock;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

/// JSON key embedded in ToolResult.output so that turn_streamed() can detect
/// and re-emit sub-agent tool calls as TurnEvents on the WebSocket.
pub const ORCHESTRATED_CALLS_KEY: &str = "__orchestrated_tool_calls";

// ---------------------------------------------------------------------------
// TrackingToolWrapper — records every tool call made by a sub-agent
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
struct TrackedToolCall {
    name: String,
    args: serde_json::Value,
    result: String,
    success: bool,
}

/// Wraps an existing tool to transparently record every invocation.
struct TrackingToolWrapper {
    inner: Arc<dyn Tool>,
    tracker: Arc<Mutex<Vec<TrackedToolCall>>>,
}

impl TrackingToolWrapper {
    fn new(inner: Arc<dyn Tool>, tracker: Arc<Mutex<Vec<TrackedToolCall>>>) -> Self {
        Self { inner, tracker }
    }
}

#[async_trait]
impl Tool for TrackingToolWrapper {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.inner.parameters_schema()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let tool_name = self.inner.name().to_string();
        let args_preview: String = args.to_string().chars().take(300).collect();
        info!(
            tool = %tool_name,
            args = %args_preview,
            "[orchestrate] SUB-TOOL '{}' called — args: {}",
            tool_name,
            args_preview
        );
        let result = self.inner.execute(args.clone()).await?;
        let output_preview: String = result.output.chars().take(300).collect();
        info!(
            tool = %tool_name,
            success = result.success,
            output_len = result.output.len(),
            "[orchestrate] SUB-TOOL '{}' result (success={}, output={}chars) preview: {}",
            tool_name,
            result.success,
            result.output.len(),
            output_preview
        );
        self.tracker.lock().push(TrackedToolCall {
            name: tool_name,
            args,
            result: result.output.clone(),
            success: result.success,
        });
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// OrchestratorTool
// ---------------------------------------------------------------------------

/// Sequential multi-agent pipeline with agentic tool-call loops.
///
/// Each agent runs in order, receiving the previous agent's output as context.
/// Tool calls made by sub-agents are tracked via [`TrackingToolWrapper`] and
/// returned as structured JSON in [`ToolResult::output`] under the key
/// [`ORCHESTRATED_CALLS_KEY`]. The main agent's `turn_streamed()` detects
/// this key and re-emits [`TurnEvent::ToolCall`] / [`TurnEvent::ToolResult`]
/// so that the WebSocket proxy can intercept them (e.g. to persist `cron_add`
/// calls to MongoDB).
pub struct OrchestratorTool {
    orchestrations: Arc<HashMap<String, OrchestrationConfig>>,
    agents: Arc<HashMap<String, DelegateAgentConfig>>,
    parent_tools: Arc<RwLock<Vec<Arc<dyn Tool>>>>,
    fallback_credential: Option<String>,
    provider_runtime_options: providers::ProviderRuntimeOptions,
    multimodal_config: crate::config::MultimodalConfig,
    delegate_config: DelegateToolConfig,
    workspace_dir: PathBuf,
    security: Arc<SecurityPolicy>,
}

impl OrchestratorTool {
    pub fn new(
        orchestrations: HashMap<String, OrchestrationConfig>,
        agents: HashMap<String, DelegateAgentConfig>,
        fallback_credential: Option<String>,
        security: Arc<SecurityPolicy>,
        provider_runtime_options: providers::ProviderRuntimeOptions,
    ) -> Self {
        Self {
            orchestrations: Arc::new(orchestrations),
            agents: Arc::new(agents),
            parent_tools: Arc::new(RwLock::new(Vec::new())),
            fallback_credential,
            provider_runtime_options,
            multimodal_config: crate::config::MultimodalConfig::default(),
            delegate_config: DelegateToolConfig::default(),
            workspace_dir: PathBuf::new(),
            security,
        }
    }

    pub fn with_parent_tools(mut self, handle: Arc<RwLock<Vec<Arc<dyn Tool>>>>) -> Self {
        self.parent_tools = handle;
        self
    }

    pub fn with_multimodal_config(mut self, config: crate::config::MultimodalConfig) -> Self {
        self.multimodal_config = config;
        self
    }

    pub fn with_delegate_config(mut self, config: DelegateToolConfig) -> Self {
        self.delegate_config = config;
        self
    }

    pub fn with_workspace_dir(mut self, dir: PathBuf) -> Self {
        self.workspace_dir = dir;
        self
    }

    // -- internal helpers ---------------------------------------------------

    fn create_provider_for_agent(
        &self,
        agent_config: &DelegateAgentConfig,
        agent_name: &str,
    ) -> Result<Box<dyn Provider>, ToolResult> {
        let credential = agent_config
            .api_key
            .clone()
            .or_else(|| self.fallback_credential.clone());

        providers::create_provider_with_options(
            &agent_config.provider,
            credential.as_deref(),
            &self.provider_runtime_options,
        )
        .map_err(|e| ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!(
                "Failed to create provider '{}' for agent '{agent_name}': {e}",
                agent_config.provider
            )),
        })
    }

    /// Filter parent tools by the agent's `allowed_tools` and wrap each in a
    /// [`TrackingToolWrapper`] so that every invocation is recorded.
    fn create_tracked_tools(
        &self,
        agent_config: &DelegateAgentConfig,
        tracker: &Arc<Mutex<Vec<TrackedToolCall>>>,
    ) -> Vec<Box<dyn Tool>> {
        let allowed: HashSet<&str> = agent_config
            .allowed_tools
            .iter()
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
            .collect();

        let parent_tools = self.parent_tools.read();
        parent_tools
            .iter()
            .filter(|tool| allowed.contains(tool.name()))
            .filter(|tool| tool.name() != "delegate" && tool.name() != "orchestrate")
            .map(|tool| {
                Box::new(TrackingToolWrapper::new(tool.clone(), Arc::clone(tracker)))
                    as Box<dyn Tool>
            })
            .collect()
    }

    /// Build a lightweight system prompt for a sub-agent.
    ///
    /// Intentionally skips `SkillsSection` and `WorkspaceSection` to keep the
    /// context small — the whole point of the orchestrator is that each agent
    /// has a tiny context so that Gemini can reliably make 1 tool call.
    fn build_enriched_system_prompt(
        &self,
        agent_config: &DelegateAgentConfig,
        sub_tools: &[Box<dyn Tool>],
        workspace_dir: &Path,
    ) -> Option<String> {
        let has_shell = sub_tools.iter().any(|t| t.name() == "shell");
        let shell_policy = if has_shell {
            "## Shell Policy\n\n\
             - Prefer non-destructive commands. Use `trash` over `rm` where possible.\n\
             - Do not run commands that exfiltrate data or modify system-critical paths.\n\
             - Avoid interactive commands that block on stdin.\n\
             - Quote paths that may contain spaces."
                .to_string()
        } else {
            String::new()
        };

        // Load filtered skills if allowed_skills is configured.
        let skills = if agent_config.allowed_skills.is_empty() {
            vec![]
        } else {
            let skills_dir = agent_config
                .skills_directory
                .as_ref()
                .filter(|s| !s.trim().is_empty())
                .map(|dir| workspace_dir.join(dir))
                .unwrap_or_else(|| crate::skills::skills_dir(workspace_dir));
            let all_skills = crate::skills::load_skills_from_directory(&skills_dir, true);
            all_skills
                .into_iter()
                .filter(|s| {
                    agent_config
                        .allowed_skills
                        .iter()
                        .any(|name| name == &s.name)
                })
                .collect()
        };

        let ctx = PromptContext {
            workspace_dir,
            model_name: &agent_config.model,
            tools: sub_tools,
            skills: &skills,
            skills_prompt_mode: crate::config::SkillsPromptInjectionMode::Full,
            identity_config: None,
            dispatcher_instructions: "",
            tool_descriptions: None,
            security_summary: None,
            autonomy_level: crate::security::AutonomyLevel::default(),
        };

        let mut builder = SystemPromptBuilder::default()
            .add_section(Box::new(crate::agent::prompt::ToolsSection))
            .add_section(Box::new(crate::agent::prompt::SafetySection))
            .add_section(Box::new(crate::agent::prompt::DateTimeSection));

        // Only add SkillsSection if there are skills to inject.
        if !skills.is_empty() {
            builder = builder.add_section(Box::new(crate::agent::prompt::SkillsSection));
        }

        let mut enriched = builder.build(&ctx).unwrap_or_default();

        if !shell_policy.is_empty() {
            enriched.push_str(&shell_policy);
            enriched.push_str("\n\n");
        }

        if let Some(operator_prompt) = agent_config.system_prompt.as_ref() {
            enriched.push_str(operator_prompt);
            enriched.push('\n');
        }

        let trimmed = enriched.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }

    /// Execute a single agent with its own agentic tool-call loop.
    async fn execute_agent(
        &self,
        agent_name: &str,
        agent_config: &DelegateAgentConfig,
        prompt: &str,
        tracker: &Arc<Mutex<Vec<TrackedToolCall>>>,
        timeout_secs: u64,
    ) -> Result<String, String> {
        let provider = self
            .create_provider_for_agent(agent_config, agent_name)
            .map_err(|r| r.error.unwrap_or_default())?;

        let temperature = agent_config.temperature.unwrap_or(0.3);
        let sub_tools = self.create_tracked_tools(agent_config, tracker);

        // Non-agentic fallback (like swarm) when no tools or agentic=false.
        if sub_tools.is_empty() || !agent_config.agentic {
            let result = tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                provider.chat_with_system(
                    agent_config.system_prompt.as_deref(),
                    prompt,
                    &agent_config.model,
                    temperature,
                ),
            )
            .await;

            return match result {
                Ok(Ok(response)) => {
                    if response.trim().is_empty() {
                        Ok("[Empty response]".to_string())
                    } else {
                        Ok(response)
                    }
                }
                Ok(Err(e)) => Err(format!("Agent '{agent_name}' failed: {e}")),
                Err(_) => Err(format!("Agent '{agent_name}' timed out after {timeout_secs}s")),
            };
        }

        // Agentic path — full tool-call loop.
        let enriched_system_prompt =
            self.build_enriched_system_prompt(agent_config, &sub_tools, &self.workspace_dir);

        let mut history = Vec::new();
        if let Some(system_prompt) = enriched_system_prompt.as_ref() {
            history.push(ChatMessage::system(system_prompt.clone()));
        }
        history.push(ChatMessage::user(prompt.to_string()));

        let noop_observer = NoopObserver;

        let agentic_timeout_secs = agent_config
            .agentic_timeout_secs
            .unwrap_or(self.delegate_config.agentic_timeout_secs);
        let effective_timeout = timeout_secs.min(agentic_timeout_secs);

        let result = tokio::time::timeout(
            Duration::from_secs(effective_timeout),
            run_tool_call_loop(
                provider.as_ref(),
                &mut history,
                &sub_tools,
                &noop_observer,
                &agent_config.provider,
                &agent_config.model,
                temperature,
                true,  // silent
                None,  // approval
                "orchestrate",
                None,  // channel_reply_target
                &self.multimodal_config,
                agent_config.max_iterations,
                None,  // cancellation_token
                None,  // on_delta
                None,  // hooks
                &[],   // excluded_tools
                &[],   // dedup_exempt_tools
                None,  // activated_tools
                None,  // model_switch_callback
                &crate::config::PacingConfig::default(),
                20000, // max_tool_result_chars: safety net for large API responses
                0,     // context_token_budget
                None,  // shared_budget
            ),
        )
        .await;

        match result {
            Ok(Ok(response)) => {
                if response.trim().is_empty() {
                    Ok("[Empty response]".to_string())
                } else {
                    Ok(response)
                }
            }
            Ok(Err(e)) => Err(format!("Agent '{agent_name}' failed: {e}")),
            Err(_) => Err(format!(
                "Agent '{agent_name}' timed out after {effective_timeout}s"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Tool trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Tool for OrchestratorTool {
    fn name(&self) -> &str {
        "orchestrate"
    }

    fn description(&self) -> &str {
        "Execute a multi-agent orchestration pipeline. Agents run sequentially, \
         each passing its output to the next. Use this for tasks that require \
         multiple steps (e.g. research → test → create scheduled job)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let orch_names: Vec<&str> = self.orchestrations.keys().map(String::as_str).collect();
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "orchestration": {
                    "type": "string",
                    "minLength": 1,
                    "description": format!(
                        "Name of the orchestration to run. Available: {}",
                        if orch_names.is_empty() {
                            "(none configured)".to_string()
                        } else {
                            orch_names.join(", ")
                        }
                    )
                },
                "prompt": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The task/prompt for the orchestration pipeline"
                }
            },
            "required": ["orchestration", "prompt"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let orch_name = args
            .get("orchestration")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .ok_or_else(|| anyhow::anyhow!("Missing 'orchestration' parameter"))?;

        if orch_name.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("'orchestration' parameter must not be empty".into()),
            });
        }

        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .ok_or_else(|| anyhow::anyhow!("Missing 'prompt' parameter"))?;

        if prompt.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("'prompt' parameter must not be empty".into()),
            });
        }

        let orch_config = match self.orchestrations.get(orch_name) {
            Some(cfg) => cfg,
            None => {
                let available: Vec<&str> =
                    self.orchestrations.keys().map(String::as_str).collect();
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Unknown orchestration '{orch_name}'. Available: {}",
                        if available.is_empty() {
                            "(none configured)".to_string()
                        } else {
                            available.join(", ")
                        }
                    )),
                });
            }
        };

        if orch_config.agents.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Orchestration '{orch_name}' has no agents configured"
                )),
            });
        }

        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "orchestrate")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        // -- Sequential execution -------------------------------------------

        info!(
            orchestration = orch_name,
            agents = ?orch_config.agents,
            prompt_len = prompt.len(),
            "[orchestrate] START orchestration '{orch_name}' with {} agents",
            orch_config.agents.len()
        );

        let tracker = Arc::new(Mutex::new(Vec::<TrackedToolCall>::new()));
        let mut current_input = prompt.to_string();
        let per_agent_timeout =
            orch_config.timeout_secs / orch_config.agents.len().max(1) as u64;

        for (i, agent_name) in orch_config.agents.iter().enumerate() {
            let agent_config = match self.agents.get(agent_name) {
                Some(cfg) => cfg,
                None => {
                    return Ok(ToolResult {
                        success: false,
                        output: json!({
                            "response": format!("Unknown agent '{agent_name}' in orchestration '{orch_name}'"),
                            ORCHESTRATED_CALLS_KEY: &*tracker.lock(),
                        })
                        .to_string(),
                        error: Some(format!(
                            "Orchestration '{orch_name}' references unknown agent '{agent_name}'"
                        )),
                    });
                }
            };

            let agent_prompt = if i == 0 {
                current_input.clone()
            } else {
                format!(
                    "[Previous step result]\n{current_input}\n\n[Original task]\n{prompt}"
                )
            };

            info!(
                agent = agent_name,
                step = i + 1,
                agentic = agent_config.agentic,
                tools = ?agent_config.allowed_tools,
                skills = ?agent_config.allowed_skills,
                "[orchestrate] AGENT {}/{} '{}' starting (agentic={}, tools={}, skills={})",
                i + 1,
                orch_config.agents.len(),
                agent_name,
                agent_config.agentic,
                agent_config.allowed_tools.len(),
                agent_config.allowed_skills.len()
            );

            match self
                .execute_agent(
                    agent_name,
                    agent_config,
                    &agent_prompt,
                    &tracker,
                    per_agent_timeout,
                )
                .await
            {
                Ok(output) => {
                    let tracked_count = tracker.lock().len();
                    let agent_output_preview: String = output.chars().take(500).collect();
                    info!(
                        agent = agent_name,
                        step = i + 1,
                        output_len = output.len(),
                        tracked_tool_calls = tracked_count,
                        "[orchestrate] AGENT {}/{} '{}' completed (output={}chars, tracked_calls={}) output_preview: {}",
                        i + 1,
                        orch_config.agents.len(),
                        agent_name,
                        output.len(),
                        tracked_count,
                        agent_output_preview
                    );
                    current_input = output;
                }
                Err(e) => {
                    info!(
                        agent = agent_name,
                        step = i + 1,
                        error = %e,
                        "[orchestrate] AGENT {}/{} '{}' FAILED: {}",
                        i + 1,
                        orch_config.agents.len(),
                        agent_name,
                        e
                    );
                    return Ok(ToolResult {
                        success: false,
                        output: json!({
                            "response": format!("Failed at agent '{agent_name}': {e}"),
                            ORCHESTRATED_CALLS_KEY: &*tracker.lock(),
                        })
                        .to_string(),
                        error: Some(e),
                    });
                }
            }
        }

        let final_tracked = tracker.lock();
        let tool_names: Vec<&str> = final_tracked.iter().map(|tc| tc.name.as_str()).collect();
        info!(
            orchestration = orch_name,
            total_tool_calls = final_tracked.len(),
            tools_called = ?tool_names,
            "[orchestrate] DONE orchestration '{orch_name}' — {} tool calls: {:?}",
            final_tracked.len(),
            tool_names
        );

        Ok(ToolResult {
            success: true,
            output: json!({
                "response": current_input,
                ORCHESTRATED_CALLS_KEY: &*final_tracked,
            })
            .to_string(),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

    fn sample_agents() -> HashMap<String, DelegateAgentConfig> {
        let mut agents = HashMap::new();
        agents.insert(
            "planner".to_string(),
            DelegateAgentConfig {
                provider: "ollama".to_string(),
                model: "llama3".to_string(),
                system_prompt: Some("You are a planner.".to_string()),
                api_key: None,
                temperature: Some(0.3),
                max_depth: 3,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 3,
                timeout_secs: None,
                agentic_timeout_secs: None,
                skills_directory: None,
                memory_namespace: None,
                allowed_skills: Vec::new(),
            },
        );
        agents.insert(
            "cron_writer".to_string(),
            DelegateAgentConfig {
                provider: "ollama".to_string(),
                model: "llama3".to_string(),
                system_prompt: Some("You create cron jobs.".to_string()),
                api_key: None,
                temperature: Some(0.3),
                max_depth: 3,
                agentic: true,
                allowed_tools: vec!["cron_add".to_string()],
                max_iterations: 3,
                timeout_secs: None,
                agentic_timeout_secs: None,
                skills_directory: None,
                memory_namespace: None,
                allowed_skills: Vec::new(),
            },
        );
        agents
    }

    fn sample_orchestrations() -> HashMap<String, OrchestrationConfig> {
        let mut orchs = HashMap::new();
        orchs.insert(
            "scheduled_job".to_string(),
            OrchestrationConfig {
                agents: vec!["planner".to_string(), "cron_writer".to_string()],
                description: Some("Plan and create scheduled job".to_string()),
                timeout_secs: 300,
            },
        );
        orchs
    }

    fn make_tool(
        orchs: HashMap<String, OrchestrationConfig>,
        agents: HashMap<String, DelegateAgentConfig>,
    ) -> OrchestratorTool {
        OrchestratorTool::new(
            orchs,
            agents,
            None,
            test_security(),
            providers::ProviderRuntimeOptions::default(),
        )
    }

    // ---- Tool trait basics ----

    #[test]
    fn name_is_orchestrate() {
        let tool = make_tool(sample_orchestrations(), sample_agents());
        assert_eq!(tool.name(), "orchestrate");
    }

    #[test]
    fn description_not_empty() {
        let tool = make_tool(sample_orchestrations(), sample_agents());
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn schema_has_required_fields() {
        let tool = make_tool(sample_orchestrations(), sample_agents());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["orchestration"].is_object());
        assert!(schema["properties"]["prompt"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("orchestration")));
        assert!(required.contains(&json!("prompt")));
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[test]
    fn schema_lists_orchestration_names() {
        let tool = make_tool(sample_orchestrations(), sample_agents());
        let schema = tool.parameters_schema();
        let desc = schema["properties"]["orchestration"]["description"]
            .as_str()
            .unwrap();
        assert!(desc.contains("scheduled_job"));
    }

    #[test]
    fn empty_orchestrations_schema() {
        let tool = make_tool(HashMap::new(), sample_agents());
        let schema = tool.parameters_schema();
        let desc = schema["properties"]["orchestration"]["description"]
            .as_str()
            .unwrap();
        assert!(desc.contains("none configured"));
    }

    // ---- Execute validation ----

    #[tokio::test]
    async fn unknown_orchestration_returns_error() {
        let tool = make_tool(sample_orchestrations(), sample_agents());
        let result = tool
            .execute(json!({"orchestration": "nonexistent", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown orchestration"));
    }

    #[tokio::test]
    async fn missing_orchestration_param() {
        let tool = make_tool(sample_orchestrations(), sample_agents());
        let result = tool.execute(json!({"prompt": "test"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn missing_prompt_param() {
        let tool = make_tool(sample_orchestrations(), sample_agents());
        let result = tool.execute(json!({"orchestration": "scheduled_job"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn blank_orchestration_rejected() {
        let tool = make_tool(sample_orchestrations(), sample_agents());
        let result = tool
            .execute(json!({"orchestration": "  ", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("must not be empty"));
    }

    #[tokio::test]
    async fn blank_prompt_rejected() {
        let tool = make_tool(sample_orchestrations(), sample_agents());
        let result = tool
            .execute(json!({"orchestration": "scheduled_job", "prompt": "  "}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("must not be empty"));
    }

    #[tokio::test]
    async fn orchestration_with_empty_agents_returns_error() {
        let mut orchs = HashMap::new();
        orchs.insert(
            "empty".to_string(),
            OrchestrationConfig {
                agents: Vec::new(),
                description: None,
                timeout_secs: 60,
            },
        );
        let tool = make_tool(orchs, sample_agents());
        let result = tool
            .execute(json!({"orchestration": "empty", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("no agents configured"));
    }

    #[tokio::test]
    async fn orchestration_with_unknown_agent_returns_error() {
        let mut orchs = HashMap::new();
        orchs.insert(
            "broken".to_string(),
            OrchestrationConfig {
                agents: vec!["nonexistent_agent".to_string()],
                description: None,
                timeout_secs: 60,
            },
        );
        let tool = make_tool(orchs, sample_agents());
        let result = tool
            .execute(json!({"orchestration": "broken", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("unknown agent"));
    }

    #[tokio::test]
    async fn orchestration_blocked_in_readonly_mode() {
        let readonly = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = OrchestratorTool::new(
            sample_orchestrations(),
            sample_agents(),
            None,
            readonly,
            providers::ProviderRuntimeOptions::default(),
        );
        let result = tool
            .execute(json!({"orchestration": "scheduled_job", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("read-only mode"));
    }

    // ---- TrackingToolWrapper ----

    #[tokio::test]
    async fn tracking_wrapper_records_calls() {
        struct DummyTool;

        #[async_trait]
        impl Tool for DummyTool {
            fn name(&self) -> &str { "dummy" }
            fn description(&self) -> &str { "test" }
            fn parameters_schema(&self) -> serde_json::Value { json!({}) }
            async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
                Ok(ToolResult {
                    success: true,
                    output: "dummy result".to_string(),
                    error: None,
                })
            }
        }

        let tracker = Arc::new(Mutex::new(Vec::new()));
        let wrapped = TrackingToolWrapper::new(
            Arc::new(DummyTool) as Arc<dyn Tool>,
            Arc::clone(&tracker),
        );

        // Verify wrapper preserves name/description
        assert_eq!(wrapped.name(), "dummy");
        assert_eq!(wrapped.description(), "test");

        // Execute and verify tracking
        let result = wrapped.execute(json!({"key": "value"})).await.unwrap();
        assert!(result.success);
        assert_eq!(result.output, "dummy result");

        let calls = tracker.lock();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "dummy");
        assert_eq!(calls[0].args, json!({"key": "value"}));
        assert_eq!(calls[0].result, "dummy result");
        assert!(calls[0].success);
    }

    #[tokio::test]
    async fn tracking_wrapper_records_multiple_calls() {
        struct CountTool(std::sync::atomic::AtomicU32);

        #[async_trait]
        impl Tool for CountTool {
            fn name(&self) -> &str { "counter" }
            fn description(&self) -> &str { "counts" }
            fn parameters_schema(&self) -> serde_json::Value { json!({}) }
            async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
                let n = self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(ToolResult {
                    success: true,
                    output: format!("call #{n}"),
                    error: None,
                })
            }
        }

        let tracker = Arc::new(Mutex::new(Vec::new()));
        let wrapped = TrackingToolWrapper::new(
            Arc::new(CountTool(std::sync::atomic::AtomicU32::new(0))) as Arc<dyn Tool>,
            Arc::clone(&tracker),
        );

        wrapped.execute(json!({"a": 1})).await.unwrap();
        wrapped.execute(json!({"b": 2})).await.unwrap();
        wrapped.execute(json!({"c": 3})).await.unwrap();

        let calls = tracker.lock();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].result, "call #0");
        assert_eq!(calls[1].result, "call #1");
        assert_eq!(calls[2].result, "call #2");
    }

    #[tokio::test]
    async fn tracking_wrapper_records_failed_tool() {
        struct FailTool;

        #[async_trait]
        impl Tool for FailTool {
            fn name(&self) -> &str { "fail" }
            fn description(&self) -> &str { "always fails" }
            fn parameters_schema(&self) -> serde_json::Value { json!({}) }
            async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
                Ok(ToolResult {
                    success: false,
                    output: "error occurred".to_string(),
                    error: Some("something broke".to_string()),
                })
            }
        }

        let tracker = Arc::new(Mutex::new(Vec::new()));
        let wrapped = TrackingToolWrapper::new(
            Arc::new(FailTool) as Arc<dyn Tool>,
            Arc::clone(&tracker),
        );

        let result = wrapped.execute(json!({})).await.unwrap();
        assert!(!result.success);

        let calls = tracker.lock();
        assert_eq!(calls.len(), 1);
        assert!(!calls[0].success);
        assert_eq!(calls[0].result, "error occurred");
    }

    // ---- ORCHESTRATED_CALLS_KEY ----

    #[test]
    fn orchestrated_calls_key_is_consistent() {
        assert_eq!(ORCHESTRATED_CALLS_KEY, "__orchestrated_tool_calls");
    }

    // ---- create_tracked_tools ----

    #[test]
    fn create_tracked_tools_filters_by_allowed() {
        struct FakeTool(&'static str);

        #[async_trait]
        impl Tool for FakeTool {
            fn name(&self) -> &str { self.0 }
            fn description(&self) -> &str { "" }
            fn parameters_schema(&self) -> serde_json::Value { json!({}) }
            async fn execute(&self, _: serde_json::Value) -> anyhow::Result<ToolResult> {
                Ok(ToolResult { success: true, output: String::new(), error: None })
            }
        }

        let parent_tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(FakeTool("shell")),
            Arc::new(FakeTool("http_request")),
            Arc::new(FakeTool("cron_add")),
            Arc::new(FakeTool("delegate")),
            Arc::new(FakeTool("orchestrate")),
        ];

        let tool = OrchestratorTool {
            orchestrations: Arc::new(HashMap::new()),
            agents: Arc::new(HashMap::new()),
            parent_tools: Arc::new(RwLock::new(parent_tools)),
            fallback_credential: None,
            provider_runtime_options: providers::ProviderRuntimeOptions::default(),
            multimodal_config: crate::config::MultimodalConfig::default(),
            delegate_config: DelegateToolConfig::default(),
            workspace_dir: PathBuf::new(),
            security: test_security(),
        };

        let tracker = Arc::new(Mutex::new(Vec::new()));

        // Agent with allowed_tools = ["shell", "cron_add"]
        let agent_config = DelegateAgentConfig {
            provider: "test".to_string(),
            model: "test".to_string(),
            system_prompt: None,
            api_key: None,
            temperature: None,
            max_depth: 3,
            agentic: true,
            allowed_tools: vec!["shell".to_string(), "cron_add".to_string()],
            max_iterations: 3,
            timeout_secs: None,
            agentic_timeout_secs: None,
            skills_directory: None,
            memory_namespace: None,
            allowed_skills: Vec::new(),
        };

        let tracked = tool.create_tracked_tools(&agent_config, &tracker);

        // Should have shell + cron_add (NOT delegate, orchestrate, or http_request)
        let names: Vec<&str> = tracked.iter().map(|t| t.name()).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"shell"));
        assert!(names.contains(&"cron_add"));
        assert!(!names.contains(&"delegate"));
        assert!(!names.contains(&"orchestrate"));
        assert!(!names.contains(&"http_request"));
    }

    // ---- Sequential provider error (non-agentic fallback) ----

    #[tokio::test]
    async fn sequential_invalid_provider_returns_error() {
        let tool = make_tool(sample_orchestrations(), sample_agents());
        // planner uses "ollama" which won't be running → should fail at provider creation
        let result = tool
            .execute(json!({"orchestration": "scheduled_job", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
    }
}
