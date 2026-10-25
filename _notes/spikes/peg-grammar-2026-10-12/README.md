# Spike: DuckDB 2.0 grammar extension (epic Phase 1), 2026-10-12

**Verdict: GO.** Every go/no-go item in `.planning/PEG-PARSER-EPIC.md` Phase 1 worked
against a real DuckDB 2.0 build, including the deciding one: transactional DDL.

The spike also produced two findings that change the plan:

1. **ABI.** `C_STRUCT_UNSTABLE` cannot load on 2.0, but a **pinned V1 C API** (`C_STRUCT`, v1.5.6) can.
2. **Expression capture.** Comments inside an expression are lost.

## Setup

- **Host:** DuckDB `v2.0.0-dev1` (`b554c96b5d`, branch `v2.0-cyanoptera`). Built from source as a release CLI, plus a C++ host harness linked against the same static libraries.
- **Extension:** `svspike.cpp` is one C++ file. Like our shim, it carries **its own statically linked copy of DuckDB**:
  - It links the 2.0 static libraries. That is the same code as the amalgamation, minus the 10-minute compile.
  - It is built with `-fvisibility=hidden`, `-Bsymbolic`, and a version script exporting only the entry point.
  - It is loaded through a C-struct entry point that unwraps `DatabaseWrapper` → `DatabaseInstance`, exactly as `sv_register_parser_hooks` does today.
- **What it registers:**
  - a `GrammarExtension` with three productions;
  - an `ExtensionCallback`;
  - a `sv_query` table function;
  - an `sv_defs` table, standing in for `semantic_layer._definitions`.

Files:

- `svspike.cpp` is the extension.
- `host.cpp` is the multi-connection harness.
- `t1.sql`, `t2.sql` and `t3.sql` are the CLI scripts.

## Results

| # | Question | Result |
|---|---|---|
| 1 | Does a `GrammarExtension` registered from our own DuckDB copy work in the host? | **Yes.** `GrammarExtension::Register(db, …)` on the host `DatabaseInstance`. The rules match, and our transforms run inside the host parser. |
| 2a | Is the grammar active on the connection that ran `LOAD`, and on connections opened **before** it? | **Yes.** At load time, iterate `db.GetConnectionManager().GetConnectionList()` and append our name to each `active_grammar_extensions`. `host.cpp` checks a connection that was opened before `LOAD`. |
| 2b | Is it active on connections opened **after** `LOAD`? | **Yes**, via `ExtensionCallback::OnConnectionOpened`. |
| 2c | What happens when a user runs `SET active_grammar_extensions = []` or `RESET`? | It turns us **off**. Our syntax then fails with DuckDB's generic `syntax error at or near "VIEWS"`. `SET … = ['svspike']` turns it back on. See "Open items". |
| 3a | **Transactional CREATE** (the go/no-go). | **Yes.** The transform lowers to SQL text, re-parses it with `Parser::GetBuiltinParser()`, and returns a `MultiStatement` (guard `SELECT`, then `INSERT`). The statements run on the caller's connection. `BEGIN; CREATE SEMANTIC VIEW v2 …; SELECT count(*)` gives 2. After `ROLLBACK` it gives 1. The duplicate guard's `error()` fires as a normal error. |
| 3b | Does `FROM SEMANTIC_VIEW(v DIMENSIONS a, b) [AS t]` work as a table ref? | **Yes.** `PrependChoice("InnerTableRef", …)` lowers it to a **stock** `TableFunctionRef` (`sv_query('v', ['a','b'])`). It works in joins, CTEs and with aliases. |
| 3c | Does `CREATE VIEW … AS SELECT * FROM SEMANTIC_VIEW(…)` work? | **Yes.** It is stored as `SELECT * FROM sv_query('v1', list_value('a', 'b'))`, and it binds on a connection with the grammar **off**. |
| 3d | Does the deprecated `semantic_view('v', dimensions := […])` still parse? | **Yes.** Our rule doesn't match a string first argument, so it falls through to the ordinary table-function path (D3). |
| 3e | Can `SHOW SEMANTIC VIEWS` be used as a subquery or a CTE? | **Yes, for free.** `PrependChoice("DescribeStatement", …)` returning a `QueryNode` works in `SELECT count(*) FROM (SHOW SEMANTIC VIEWS)` and `WITH s AS (SHOW SEMANTIC VIEWS) …`. That closes TECH-DEBT #77's mechanism. |
| 4 | What does a syntax error inside our rule look like? | DuckDB's generic, but **correctly positioned**: `syntax error at or near ")"` with a caret under the offending token. There is no rule-local wording. |
| 5 | Can the transform recover expression source text? | **Mostly.** The transformer has no query string, but `transformer.token_iterator` exposes every token's original `text` and `offset`. Rebuilding with the original gap widths gives exact text for code, including spacing, quoting, `E'…'` and `''`. **Comments inside an expression are lost**: `a /* comment */ + 1` comes back as `a               + 1`. The gap is preserved as spaces. |

## Findings that change the plan

1. **`C_STRUCT_UNSTABLE` does not load on 2.0.**
   - 2.0 requires an unstable or v2 C-API extension to export `<name>_init_c_api_v2(duckdb_v2_extension_input*)`, which is a new handle-based API.
   - However, **a `C_STRUCT` extension pinned to the V1 C API at v1.5.6 loads fine on 2.0**, through the same `<name>_init_c_api(info, access)` entry and `get_database`. That is what the spike used.
   - Implication for Phase 2: switch the ABI type from `C_STRUCT_UNSTABLE` to `C_STRUCT` with a v1.5.6 API floor. The research note says everything unstable in V1 was stabilised into 1.5.6.
   - Then **duckdb-rs `=1.10506.0` bindings can keep working against a 2.0 host**. That removes the hard dependency on a duckdb-rs 2.0 release (spike item 6), but it has to be verified with the real crate.
2. **`C_STRUCT` turns off DuckDB's exact-version check, but our C++ still needs an exact match.**
   - Under `C_STRUCT_UNSTABLE`, DuckDB refused a version mismatch. Under `C_STRUCT`, it will load us into any 2.0.x host.
   - Our C++ (shim, grammar transforms) reads host objects by layout, so a mismatch is undefined behaviour.
   - We need our own load-time guard: compare the host's version or source id with the one our amalgamation was built from, and refuse to load on a mismatch.
3. **Expression text capture loses comments (D4).**
   - Code text is verbatim, so `GET_DDL` fidelity holds for everything except comments inside an expression.
   - Options:
     - accept it and document it;
     - recover comments from the gap bytes if the host's query string can be reached later, e.g. `ClientContext::GetCurrentQuery()` in a bind-time step;
     - store the token-rebuilt text and treat comment loss as a known divergence.
   - Recommendation: accept and document it.
4. **Statements become a `MultiStatement`.**
   - The CLI shows the guard `SELECT`'s result before the `INSERT`'s. Today's text rewrite does the same.
   - Phase 3 can fold the guards into the write, e.g. `INSERT … SELECT … WHERE` plus `error()`, so each DDL statement is one statement with one result.

## Open items for Phase 3 (not blockers)

- **Grammar turned off on a connection.**
  - A user's `SET active_grammar_extensions = [...]` or `RESET` silently removes us, and the error is a generic syntax error.
  - Fix: keep a minimal token-based `parse_function` fallback (2.0's `ParserExtension`) that recognises `CREATE SEMANTIC` / `SHOW SEMANTIC` / `SEMANTIC_VIEW(` and returns `consumed_tokens < 0` with a message such as "semantic_views syntax is not active on this connection; run `SET active_grammar_extensions = [..., 'semantic_views']`".
  - Document the coexistence rule with other grammar extensions.
- **Thread safety.** Setting `active_grammar_extensions` on other connections at load time, from the loading thread, needs a look for races with connections that are running queries.
- **Positioned semantic errors** from inside a transform (`ParserException` with a `QueryLocation`) were not exercised.
- **Python client:** not available in this environment. The visibility trap from Phase 15 (`_notes/architecture.md`, "Entry point and C++ linkage") was a Python-only failure, so check it as soon as a 2.0 Python wheel exists.
- **The real Rust crate** has not been built against a 2.0 host. The spike is C++ only. The FFI and C-API surface is unchanged in kind, but finding 1 has to be verified with duckdb-rs.

## Reproduce

1. Build the host:
   ```sh
   git clone --depth 1 -b v2.0-cyanoptera https://github.com/duckdb/duckdb
   cmake -G Ninja -DCMAKE_BUILD_TYPE=Release -DBUILD_EXTENSIONS="" -DBUILD_UNITTESTS=0 ..
   ninja shell
   ```
2. Build the extension:
   ```sh
   g++ -std=c++17 -O1 -fPIC -fvisibility=hidden -shared svspike.cpp -o svspike.so \
       -I<duckdb>/src/include -I<third_party includes> \
       -Wl,--version-script=exports.map -Wl,-Bsymbolic \
       -Wl,--start-group <all build/*.a except shell> -Wl,--end-group
   ```
   `exports.map` contains `{ global: svspike_init_c_api; local: *; };`.
3. Add metadata:
   ```sh
   cmake -DEXTENSION=svspike.duckdb_extension -DABI_TYPE=C_STRUCT -DVERSION_FIELD=v1.5.6 … \
         -P scripts/append_metadata.cmake
   ```
4. Run the scripts:
   ```sh
   duckdb -unsigned -c ".read t1.sql"
   ```
5. For the multi-connection checks, build and run `host.cpp`. It must be linked with the shell's static-extension loader objects, and with `libduckdb_loadable_extensions.a` linked `--whole-archive`.
