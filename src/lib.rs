//! stationd — library crate.
//!
//! Holds all the shared logic (config loading, SQLite persistence, playlist
//! model/parsing/validation, gRPC service) so it can be driven both by the
//! `stationd` binary (`src/main.rs`) and, crucially, by integration tests in
//! `tests/`, which can only see a *library* crate — not a binary.
//!
//! The binary is a thin shell around this library. Anything Linux-only (the
//! `signal::unix` shutdown handling) stays in `main.rs`, deliberately kept
//! out of here so the library itself stays portable.

pub mod clock;
pub mod config;
pub mod db;
pub mod grid_engine;
pub mod grid_index;
pub mod grid_store;
pub mod grid_toml;
pub mod group_state;
pub mod grpc;
pub mod library_actor;
pub mod library_grpc;
pub mod media;
pub mod media_index;
pub mod playlist;
pub mod playlist_cursor;
pub mod proto;
pub mod resolver;
pub mod schedule_grpc;
pub mod selection;
pub mod store;
pub mod sync;
