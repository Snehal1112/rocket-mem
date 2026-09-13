pub mod config;
pub mod pool;

// `errors` and `server` are added here by Task 2, which creates
// `src/errors.rs` and `src/server.rs`. Declaring them before those files
// exist makes this crate fail to compile even after Task 1's own pieces
// (`config`, `pool`) are implemented — see task-1-report.md's "Deviations
// from the brief" for details.
