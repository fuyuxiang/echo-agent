//! Memory system shim.
//!
//! The memory "core engine" now lives in the standalone `echo-agent-memory`
//! crate. This module re-exports that crate's public surface under the
//! historical `crate::session::memory::*` paths so the ~30 reverse-dependency
//! call sites in this crate keep compiling unchanged.
//!
//! Only `hooks` stays here: it is session glue (depends on
//! `crate::sampling` and `crate::session::helpers::session_compact`) and is
//! not part of the relocatable core engine.

pub mod background;
pub mod hooks;
pub mod pending;

pub use echo_agent_memory::{
    EndpointScopedCredentials, MemoryBackendImpl, MemoryBackendParams, MemoryIndex, MemoryScope,
    MemorySearchSource, MemoryStorage, archive, backend, chunker, dream, dream_lock,
    embed_missing_chunks, embedding, flush, index, init_sqlite_vec, mmr,
    noop_memory_observation_sink, query_expansion, schema, search, storage, text_utils, watcher,
};
