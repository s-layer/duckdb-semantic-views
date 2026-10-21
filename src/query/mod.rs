#[cfg(feature = "extension")]
pub mod error;
#[cfg(feature = "extension")]
pub mod explain;
#[cfg(feature = "extension")]
pub mod table_function;

// Pure wire-format / SQL-shape helpers, always compiled so they are covered by
// the default `cargo test` / clippy / coverage runs even though the FFI
// entrypoints that call them are `extension`-gated (TC-8).
pub mod wire;

// The items of a request (`<member> [[AS] <alias>]`) and the column layout
// they ask for; pure, so always compiled.
pub mod items;

// Raw C-API query helpers (schema inference, DESCRIBE, VARCHAR reads) shared by
// `table_function`, `explain` and `member_types`; always compiled so `cargo
// test` drives them against the bundled DuckDB.
pub mod raw_sql;

// Read-side member type probing for SHOW / DESCRIBE `data_type` (TECH-DEBT #51).
pub mod member_types;
