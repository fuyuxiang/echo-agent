//! Subagent configuration resolution crate.
//!
//! Extracts the pure-logic "resolution" phase of subagent spawning from
//! `echo-agent-runtime` into a reusable library. Given a spawn request and a
//! resolution context (roles, personas, parent state), this crate resolves:
//!
//! - Effective runtime config (model, persona, capability mode, isolation)
//!   via precedence: explicit override > role > persona > parent.
//! - Persona instruction loading (inline `instructions` + `instructions_file`).
//! - Role prompt file loading.
//! - Resume identity validation (type/persona match checks; model is soft-ignored).
//!
//! This crate has no dependency on session, coordinator, or transport types.
//! Designed to be consumed by local hosts (e.g. `echo-agent-runtime`) and any
//! future remote spawn path that only needs pure resolution logic.
//!
//! Definition discovery, gating, prompt context, runtime defaults, and
//! capability/depth tool policy are shared here. Model catalog selection and
//! workspace materialization remain host adapters.

pub mod config;
pub mod context;
pub mod definition;
pub mod overrides;
pub mod resume;
pub mod types;

pub use config::{PersonaIOField, SubagentPersona, SubagentRole};
pub use definition::{
    DefinitionResolutionContext, DefinitionValidationContext, HarnessToolsetContext,
    apply_child_tool_policy, apply_definition_runtime_defaults, apply_harness_toolset,
    available_agent_names, discover_agent_definition, gate_agent_definition,
    render_subagent_initial_user_message, render_subagent_system_prompt, resolve_agent_definition,
    resolve_runtime_config, select_role, subagent_harness_flavor_is_representable,
    validate_agent_name,
};
pub use echo_agent_core::config::AgentDefinition;
pub use overrides::{intersect_capability_modes, resolve_effective_overrides};
pub use resume::{ResumeValidationError, validate_resume_identity};
pub use types::{ContextSource, EffectiveRuntimeConfig, ResolutionError, ResumeSourceData};
