// Parser extension type declarations for DuckDB >= 1.5.0 compatibility, plus
// the small set of version-compat helpers the shim uses.
//
// SV_DUCKDB_2
// -----------
// The shim compiles against two DuckDB generations from one source:
//
//   * default (macro undefined): the DuckDB v1.5.x single-file amalgamation
//     header `duckdb.hpp` in cpp/include/. That header lacks several types
//     that only exist in duckdb.cpp, so this file re-declares them (below).
//   * `-DSV_DUCKDB_2`: DuckDB 2.0, which ships no amalgamation. The shim is
//     compiled against the engine's own header tree (`src/include` of the
//     matching engine commit) and linked against DuckDB's static libraries.
//     The full header tree has every type, so the real headers are included
//     and nothing is re-declared.
//
// SV_DUCKDB_2 is the only switch. Every 2.0 difference is either confined to
// this file (type aliases and `sv_compat::` helpers at the bottom) or is a
// small `#ifdef SV_DUCKDB_2` block in shim.cpp where the two APIs cannot be
// expressed through one helper. The v1.5 branches are the pre-existing code,
// unchanged.
//
// Since v0.15.0 build.rs always defines SV_DUCKDB_2 (it links a 2.0 engine
// built by scripts/ensure_engine.py), so the v1.5 branches are no longer
// compiled. They stay until the switch is removed (TECH-DEBT #91).
//
// ---------------------------------------------------------------------------
// v1.5 amalgamation notes
// ---------------------------------------------------------------------------
//
// In DuckDB 1.5.0, the parser extension types (ParserExtensionParseData,
// ParserExtensionParseResult, ParserExtensionPlanResult, ParserExtension,
// ExtensionCallbackManager) were moved from the amalgamation header
// (duckdb.hpp) into the source (duckdb.cpp). Since shim.cpp is compiled
// as a separate translation unit from duckdb.cpp, it no longer sees these
// declarations via `#include "duckdb.hpp"`.
//
// This header re-declares the types needed by shim.cpp. The definitions
// are extracted verbatim from the amalgamation source
// (`duckdb/parser/parser_extension.hpp` section of duckdb.cpp, lines
// 23732-23837, and `duckdb/main/extension_callback_manager.hpp` section).
//
// IMPORTANT: These declarations must EXACTLY match duckdb.cpp to avoid
// ODR (One Definition Rule) violations. When DuckDB is updated, verify
// these declarations still match by:
//   grep -A 120 'duckdb/parser/parser_extension.hpp' cpp/include/duckdb.cpp
//
// As of v0.8.0 the extension only uses parser_override (the legacy
// parse_function/plan_function path was retired in the full unification),
// but the ParserExtension class retains its parse_function and plan_function
// fields under ODR — we keep their typedefs and supporting structs for
// layout compatibility even though shim.cpp never assigns to them.
//
// AllowParserOverride enum, full ParserOptions struct, and minimal Parser
// class declaration are also re-declared so shim.cpp can re-parse
// rewritten SQL produced by the parser_override callback. Definitions
// match duckdb.cpp lines ~23790, ~23800, ~23949.

#pragma once

#include <chrono>
#include <cstddef>
#include <type_traits>

#ifdef SV_DUCKDB_2

// DuckDB 2.0: the real declarations. capi_internal.hpp provides
// `DatabaseWrapper` (what a C-API `duckdb_database` points at), which v1.5's
// amalgamation header exposed directly.
#include "duckdb.hpp"
#include "duckdb/common/error_data.hpp"
#include "duckdb/catalog/catalog.hpp"
#include "duckdb/catalog/catalog_search_path.hpp"
#include "duckdb/main/capi/capi_internal.hpp"
#include "duckdb/main/client_config.hpp"
#include "duckdb/main/client_data.hpp"
#include "duckdb/main/connection_manager.hpp"
#include "duckdb/main/extension_callback_manager.hpp"
#include "duckdb/main/settings.hpp"
#include "duckdb/parser/grammar_extension.hpp"
#include "duckdb/parser/parsed_data/create_scalar_function_info.hpp"
#include "duckdb/parser/parsed_data/create_table_function_info.hpp"
#include "duckdb/parser/parser.hpp"
#include "duckdb/parser/parser_extension.hpp"
#include "duckdb/parser/peg/transformer/parse_result.hpp"
#include "duckdb/parser/peg/passthrough_dialect.hpp"
#include "duckdb/parser/peg/transformer/peg_transformer.hpp"
#include "duckdb/parser/query_error_context.hpp"
#include "duckdb/parser/statement/multi_statement.hpp"
#include "duckdb/parser/expression/subquery_expression.hpp"
#include "duckdb/parser/parsed_expression_iterator.hpp"
#include "duckdb/parser/statement/select_statement.hpp"
#include "duckdb/parser/tableref/subqueryref.hpp"
#include "duckdb/parser/tableref/table_function_ref.hpp"
#include "duckdb/planner/binder.hpp"
#include "duckdb/planner/extension_callback.hpp"

#else // v1.5 amalgamation

#include "duckdb.hpp"

namespace duckdb {

// Forward declarations
class ExtensionCallbackManager;
class ExtensionCallbackRegistry;
class ParserExtension;

//===--------------------------------------------------------------------===//
// ExtensionCallbackIteratorHelper<T> (Phase 65.1 Plan 12 — WR-09 D-21)
//===--------------------------------------------------------------------===//
// Minimal re-declaration mirroring `duckdb.cpp:120801-120818`. Layout must
// match the amalgamation under ODR: { const vector<T> &, shared_ptr<...> }.
// We only need the `begin()`/`end()` iterators (inline) to support
// range-for over `ParserExtensions()` from the WR-09 idempotence check.
// The constructor/destructor are non-inline in the amalgamation; we only
// receive instances by value from `ParserExtensions()` so we don't need
// to construct them ourselves.
template <class T>
class ExtensionCallbackIteratorHelper {
public:
	ExtensionCallbackIteratorHelper(const vector<T> &vec, shared_ptr<ExtensionCallbackRegistry> callback_registry);
	~ExtensionCallbackIteratorHelper();

private:
	const vector<T> &vec;
	shared_ptr<ExtensionCallbackRegistry> callback_registry;

public:
	typename vector<T>::const_iterator begin() { // NOLINT: match stl API
		return vec.cbegin();
	}
	typename vector<T>::const_iterator end() { // NOLINT: match stl API
		return vec.cend();
	}
};

//===--------------------------------------------------------------------===//
// ParserExtensionInfo
//===--------------------------------------------------------------------===//
struct ParserExtensionInfo {
	virtual ~ParserExtensionInfo() {
	}
};

//===--------------------------------------------------------------------===//
// Parse
//===--------------------------------------------------------------------===//
enum class ParserExtensionResultType : uint8_t {
	PARSE_SUCCESSFUL,
	DISPLAY_ORIGINAL_ERROR,
	DISPLAY_EXTENSION_ERROR
};

struct ParserExtensionParseData {
	virtual ~ParserExtensionParseData() {
	}

	virtual unique_ptr<ParserExtensionParseData> Copy() const = 0;
	virtual string ToString() const = 0;
};

struct ParserExtensionParseResult {
	ParserExtensionParseResult() : type(ParserExtensionResultType::DISPLAY_ORIGINAL_ERROR) {
	}
	explicit ParserExtensionParseResult(string error_p)
	    : type(ParserExtensionResultType::DISPLAY_EXTENSION_ERROR), error(std::move(error_p)) {
	}
	explicit ParserExtensionParseResult(unique_ptr<ParserExtensionParseData> parse_data_p)
	    : type(ParserExtensionResultType::PARSE_SUCCESSFUL), parse_data(std::move(parse_data_p)) {
	}

	//! Whether or not parsing was successful
	ParserExtensionResultType type;
	//! The parse data (if successful)
	unique_ptr<ParserExtensionParseData> parse_data;
	//! The error message (if unsuccessful)
	string error;
	//! The error location (if unsuccessful)
	optional_idx error_location;
};

// Phase 62 layout guard — sv_parse_stub writes result.error_location after
// construction (the constructors don't take it as a parameter). If a future
// DuckDB bump moves error_location to private or changes its type from
// optional_idx, this assertion fires before silent breakage. See
// `.planning/phases/62-caret-restoration-lru-removal/62-RESEARCH.md` Risk F.
//
// The size upper bound is intentionally loose to tolerate ABI padding
// differences across compilers — its purpose is to catch a *jump* (e.g. a
// new field added) not to lock the exact byte count. The type-check on
// error_location is the strict guard.
static_assert(sizeof(ParserExtensionParseResult) <= 64,
              "ParserExtensionParseResult layout drift -- re-grep duckdb.cpp parser_extension.hpp");
static_assert(std::is_same<decltype(ParserExtensionParseResult{}.error_location), optional_idx>::value,
              "ParserExtensionParseResult::error_location type drift");

typedef ParserExtensionParseResult (*parse_function_t)(ParserExtensionInfo *info, const string &query);

//===--------------------------------------------------------------------===//
// Plan
//===--------------------------------------------------------------------===//
struct ParserExtensionPlanResult { // NOLINT: work-around bug in clang-tidy
	//! The table function to execute
	TableFunction function;
	//! Parameters to the function
	vector<Value> parameters;
	//! The set of databases that will be modified by this statement (empty for a read-only statement)
	unordered_map<string, StatementProperties::ModificationInfo> modified_databases;
	//! Whether or not the statement requires a valid transaction to be executed
	bool requires_valid_transaction = true;
	//! What type of result set the statement returns
	StatementReturnType return_type = StatementReturnType::NOTHING;
};

typedef ParserExtensionPlanResult (*plan_function_t)(ParserExtensionInfo *info, ClientContext &context,
                                                     unique_ptr<ParserExtensionParseData> parse_data);

//===--------------------------------------------------------------------===//
// Parser override
//===--------------------------------------------------------------------===//
struct ParserOverrideResult {
	explicit ParserOverrideResult() : type(ParserExtensionResultType::DISPLAY_ORIGINAL_ERROR) {};

	explicit ParserOverrideResult(vector<unique_ptr<SQLStatement>> statements_p)
	    : type(ParserExtensionResultType::PARSE_SUCCESSFUL), statements(std::move(statements_p)) {};

	explicit ParserOverrideResult(std::exception &error_p)
	    : type(ParserExtensionResultType::DISPLAY_EXTENSION_ERROR), error(error_p) {};

	ParserExtensionResultType type;
	vector<unique_ptr<SQLStatement>> statements;
	ErrorData error;
};

//===--------------------------------------------------------------------===//
// AllowParserOverride
//===--------------------------------------------------------------------===//
enum class AllowParserOverride : uint8_t { DEFAULT_OVERRIDE, FALLBACK_OVERRIDE, STRICT_OVERRIDE };

//===--------------------------------------------------------------------===//
// ParserOptions
//===--------------------------------------------------------------------===//
struct ParserOptions {
	bool preserve_identifier_case = true;
	bool integer_division = false;
	idx_t max_expression_depth = 1000;
	optional_ptr<const ExtensionCallbackManager> extensions;
	AllowParserOverride parser_override_setting = AllowParserOverride::DEFAULT_OVERRIDE;
};

// Guard against silent layout drift between this redeclaration and duckdb.cpp.
// On DuckDB v1.5.2 (duckdb-rs crate `=1.10502.0`, pinned in Cargo.toml) the layout
// is { bool, bool, idx_t, optional_ptr<...>, AllowParserOverride } and packs
// to 32 bytes on a 64-bit target (alignof 8 forces 6B pad after the bools,
// 7B trailing pad after the enum). If a DuckDB bump changes the field set
// this assert fires; re-grep parser.hpp / parser_options.hpp in duckdb.cpp
// and adjust both the struct and this constant in lockstep. Truncating
// ParserOptions previously caused garbage parse errors like
// `syntax error at or near "" position 0` (see milestone v0.8.0 commit 55ddcda).
static_assert(sizeof(ParserOptions) == 32,
              "ParserOptions layout drift -- re-grep duckdb.cpp parser_options.hpp");

typedef ParserOverrideResult (*parser_override_function_t)(ParserExtensionInfo *info, const string &query,
                                                           ParserOptions &options);

//===--------------------------------------------------------------------===//
// ParserExtension
//===--------------------------------------------------------------------===//
class ParserExtension {
public:
	//! The parse function of the parser extension.
	//! Takes a query string as input and returns ParserExtensionParseData (on success) or an error
	parse_function_t parse_function = nullptr;

	//! The plan function of the parser extension
	//! Takes as input the result of the parse_function, and outputs various properties of the resulting plan
	plan_function_t plan_function = nullptr;

	//! Override the current parser with a new parser and return a vector of SQL statements
	parser_override_function_t parser_override = nullptr;

	//! Additional parser info passed to the parse function
	shared_ptr<ParserExtensionInfo> parser_info;

	static void Register(DBConfig &config, ParserExtension extension);
};

//===--------------------------------------------------------------------===//
// ExtensionCallbackManager
//===--------------------------------------------------------------------===//
// Phase 65.1 Plan 12 (WR-09 D-21): extended with `ParserExtensions()` to
// expose the public read-side iterator the WR-09 idempotence check uses
// before calling `ParserExtension::Register`. Matches the public surface
// at `duckdb.cpp:120772-120795` for the methods we link against. We do
// not re-declare the full private layout (mutex + shared_ptr<registry>);
// shim.cpp only obtains references via `DBConfig::GetCallbackManager()`
// and never constructs one itself, so layout-faithfulness is unnecessary.
class ExtensionCallbackManager {
public:
	static ExtensionCallbackManager &Get(DatabaseInstance &db);
	void Register(ParserExtension extension);
	ExtensionCallbackIteratorHelper<ParserExtension> ParserExtensions() const;
	bool HasParserExtensions() const;
};

//===--------------------------------------------------------------------===//
// CreateScalarFunctionInfo — Phase 65 Plan 05 (Task 2 Step A) compat shim
//===--------------------------------------------------------------------===//
//
// As of DuckDB v1.5.2 the `CreateScalarFunctionInfo` struct is forward-declared
// in `duckdb.hpp` (lines 41536, 54050) but its body lives in `duckdb.cpp`
// (line 2539). Because `shim.cpp` is compiled as a separate translation unit
// from the amalgamation source, the forward declaration alone is not enough
// for `sv_register_scalar_function` to construct a value. We re-declare the
// struct here verbatim from `duckdb.cpp:2539-2548`.
//
// IMPORTANT: keep this in lockstep with the amalgamation. If a DuckDB bump
// changes the struct (e.g. adds a field) the linker will accept it (ODR
// allows multiple declarations of the same type) but the constructed value
// will have wrong layout — silent corruption. Re-grep when bumping DuckDB:
//   grep -nE 'struct CreateScalarFunctionInfo' .amalgamation/<ver>/duckdb.cpp
struct CreateScalarFunctionInfo : public CreateFunctionInfo {
    DUCKDB_API explicit CreateScalarFunctionInfo(ScalarFunction function);
    DUCKDB_API explicit CreateScalarFunctionInfo(ScalarFunctionSet set);

    ScalarFunctionSet functions;

public:
    DUCKDB_API unique_ptr<CreateInfo> Copy() const override;
    DUCKDB_API unique_ptr<AlterInfo> GetAlterInfo() const override;
};

//===--------------------------------------------------------------------===//
// Parser (minimal — only the surface shim.cpp uses to re-parse rewritten SQL)
//===--------------------------------------------------------------------===//
//
// Layout MUST mirror duckdb::Parser in the linked amalgamation exactly,
// including the trailing private `options` field. Truncating it makes
// `Parser parser;` allocate too little storage and the constructor / ParseQuery
// will write past the object — observed as garbage parse errors like
// `syntax error at or near "" position 0`. Keep this in sync with
// duckdb.cpp's class Parser declaration when bumping DuckDB.
class Parser {
public:
	explicit Parser(ParserOptions options = ParserOptions());

	vector<unique_ptr<SQLStatement>> statements;

public:
	void ParseQuery(const string &query);

private:
	ParserOptions options;
};

} // namespace duckdb

#endif // SV_DUCKDB_2

//===--------------------------------------------------------------------===//
// sv_compat — helpers that spell one operation for both DuckDB generations
//===--------------------------------------------------------------------===//
// Each helper's v1.5 branch is exactly the expression shim.cpp used before the
// 2.0 port; the 2.0 branch is the replacement API, with a note on why it is
// equivalent.
namespace sv_compat {

// Whether `context`'s current query has been interrupted (duckdb_interrupt,
// Connection::Interrupt, Ctrl-C).
//
// v1.5: the public `atomic<bool> ClientContext::interrupted`, loaded with
// `order` (callers pass what they used before).
// 2.0: the flag became `atomic<ClientInterruptState> interrupt_state`
// (NOT_INTERRUPTED / INTERRUPTED / INTERRUPTS_SUPPRESSED) and is read through
// `IsInterrupted()`, a relaxed load comparing against INTERRUPTED. It is set
// by `ClientContext::Interrupt()` (what Connection::Interrupt and
// duckdb_interrupt call) and cleared at the same points v1.5 cleared
// `interrupted` (InitialCleanup, new autocommit transaction, failed
// statement). INTERRUPTS_SUPPRESSED, which DuckDB enters after an irreversible
// step such as COMMIT, reads as "not interrupted", which is the point of it.
// `order` has no 2.0 counterpart; a single polled flag carries no data, so the
// relaxed load is equivalent for every caller here.
//
inline bool is_interrupted(const duckdb::ClientContext &context,
                           std::memory_order order = std::memory_order_seq_cst) {
#ifdef SV_DUCKDB_2
	(void)order;
	return context.IsInterrupted();
#else
	return context.interrupted.load(order);
#endif
}

// Whether `context`'s current query has run past its deadline.
//
// v1.5: there is no deadline.
// 2.0: `SET max_execution_time` gives each query a deadline
// (`ClientContext::query_deadline`, milliseconds on the steady clock, set when
// the query begins). The engine enforces it in `InterruptCheck()`, apart from
// the interrupt flag, so it never reaches a probe connection the shim opens:
// those are separate ClientContexts, and the setting is the caller's. Checking
// it next to the flag lets the shim forward and raise it the same way.
inline bool deadline_passed(const duckdb::ClientContext &context) {
#ifdef SV_DUCKDB_2
	if (!context.query_deadline.IsValid()) {
		return false;
	}
	auto now = std::chrono::duration_cast<std::chrono::milliseconds>(
	               std::chrono::steady_clock::now().time_since_epoch())
	               .count();
	return static_cast<duckdb::idx_t>(now) >= context.query_deadline.GetIndex();
#else
	(void)context;
	return false;
#endif
}

// Whether `context`'s current query has been cancelled, by an interrupt or
// by its deadline: the condition to forward to a probe connection.
inline bool is_cancelled(const duckdb::ClientContext &context,
                         std::memory_order order = std::memory_order_seq_cst) {
	return is_interrupted(context, order) || deadline_passed(context);
}

// Throws what the engine itself throws for a cancelled `context`: a bare
// InterruptException ("Interrupted!") for an interrupt, and one carrying the
// engine's deadline message for a deadline (`ClientContext::InterruptCheck`).
inline void throw_if_cancelled(const duckdb::ClientContext &context,
                               std::memory_order order = std::memory_order_seq_cst) {
	if (is_interrupted(context, order)) {
		throw duckdb::InterruptException();
	}
	if (deadline_passed(context)) {
		throw duckdb::InterruptException("Query exceeded maximum execution time");
	}
}

// The output-column-names vector a table-function bind callback fills
// (`table_function_bind_t`'s last parameter).
//
// v1.5: `vector<string>`.
// 2.0: `vector<Identifier>`. An Identifier stores the string as given (case
// preserved; `GetIdentifierName()` returns it unchanged) and only compares and
// hashes case-insensitively. The binder already matched column names
// case-insensitively in v1.5 (BindContext / `case_insensitive_set_t` in
// BindTableFunctionInternal), so the emitted column names and their lookup
// behave the same. Construction from `const char *` is implicit and from
// `std::string` explicit, so callers append with `emplace_back`, which works
// for both element types.
#ifdef SV_DUCKDB_2
using bind_names_t = duckdb::vector<duckdb::Identifier>;
#else
using bind_names_t = duckdb::vector<duckdb::string>;
#endif

// Writable access to a flat vector's data and validity mask.
//
// v1.5: `FlatVector::GetData<T>` / `FlatVector::Validity` return mutable
// references for a non-const Vector.
// 2.0: those accessors return `const T *` / `const ValidityMask &`; writing
// goes through `GetDataMutable<T>` / `ValidityMutable`, which return the same
// buffer and mask (`Vector::BufferMutable()` vs `Buffer()` on the one
// `buffer` member, no copy-on-write) after the same flat/constant checks.
template <class T>
inline T *flat_data_mut(duckdb::Vector &vector) {
#ifdef SV_DUCKDB_2
	return duckdb::FlatVector::GetDataMutable<T>(vector);
#else
	return duckdb::FlatVector::GetData<T>(vector);
#endif
}

inline duckdb::ValidityMask &validity_mut(duckdb::Vector &vector) {
#ifdef SV_DUCKDB_2
	return duckdb::FlatVector::ValidityMutable(vector);
#else
	return duckdb::FlatVector::Validity(vector);
#endif
}

// A parser with the built-in grammar and no parser extensions, so re-parsing
// the SQL our parser_override produced cannot re-enter parser_override. v1.5:
// default-constructed ParserOptions; 2.0 dropped the default constructor for
// `Parser::GetBuiltinParser()`.
inline duckdb::Parser builtin_parser() {
#ifdef SV_DUCKDB_2
	return duckdb::Parser::GetBuiltinParser();
#else
	return duckdb::Parser();
#endif
}

// Function registration.
//
// A function's name: v1.5 `string`; 2.0 `Identifier` (explicitly constructed
// from a runtime string; the stored name is the string as given).
#ifdef SV_DUCKDB_2
using function_name_t = duckdb::Identifier;
#else
using function_name_t = duckdb::string;
#endif

inline function_name_t function_name(const std::string &name) {
	return function_name_t(name);
}

// Declare an optional named parameter `name := <type>` on a table function.
//
// v1.5: an entry in `TableFunction::named_parameters` (a case-insensitive
// name -> type map). The binder rejects names not in the map ("Invalid named
// parameter"), casts a passed value with DefaultCastAs, and hands only the
// parameters the call passed to the bind callback, in
// `TableFunctionBindInput::named_parameters` (case-insensitive lookup).
// 2.0: `named_parameters` is gone; a function's parameters live in its
// `FunctionSignature`. The equivalent of the v1.5 map is the schema of a typed
// "**kwargs" parameter (`TypedKwargs`), which is what DuckDB's own table
// functions (read_csv, duckdb_settings, ...) moved their v1.5 named parameters
// to: an option the call leaves out is not passed, a passed one is cast to the
// option's type like an explicit cast (`PlaceOption` -> TryCastAs), an unknown
// name is rejected with "Invalid named parameter", and the bind callback finds
// it in `TableFunctionBindInput::named_parameters` keyed by the option's name
// (Identifier: case-insensitive). The "**options" parameter itself is variadic,
// so duckdb_functions() does not list it; it lists the options after the
// positional parameters, in the order they were added.
// (A keyword-only parameter would not do: without a default it is required,
// and with one the bind callback would always receive it.)
inline void add_named_parameter(duckdb::TableFunction &function, const std::string &name,
                                const duckdb::LogicalType &type) {
#ifdef SV_DUCKDB_2
	auto add = [&](duckdb::TypedKwargs &options) { options.Add(duckdb::Identifier(name), type); };
	auto &signature = function.GetSignature();
	if (signature.GetTypedKwargs()) {
		signature.ExtendTypedKwargs(add);
	} else {
		signature.WithTypedKwargs("options", add);
	}
#else
	function.named_parameters[name] = type;
#endif
}

// The names of `info`'s (single) table function's named parameters, in the
// order duckdb_functions() renders them after the positional parameters.
//
// v1.5: duckdb_functions() iterates `named_parameters` (an unordered_map) of a
// by-value copy of the catalog entry's function, and libc++ reverses that
// iteration order on every copy; a `GetFunctionByOffset` copy of the info's
// function iterates exactly like DuckDB's rendering copy (see the call site in
// shim.cpp for the full reasoning).
// 2.0: the options of the "**kwargs" schema, in insertion order, which is
// exactly what duckdb_functions() (`AddOptions`) lists.
inline duckdb::vector<std::string> rendered_named_parameters(duckdb::CreateTableFunctionInfo &info) {
	duckdb::vector<std::string> names;
#ifdef SV_DUCKDB_2
	auto options = info.functions.GetFunctionByOffset(0)->GetSignature().GetTypedKwargs();
	if (options) {
		for (const auto &option : options->GetOptions()) {
			names.push_back(option.name.GetIdentifierName());
		}
	}
#else
	const duckdb::TableFunction rendered = info.functions.GetFunctionByOffset(0);
	for (const auto &np : rendered.named_parameters) {
		names.push_back(np.first);
	}
#endif
	return names;
}

// Give a function's positional parameters the documented names.
//
// v1.5: positional parameters are bare types; duckdb_functions() takes their
// names from the FunctionDescription's `parameter_names`, so this is a no-op.
// 2.0: positional parameters are named in the signature (`col0`, `col1`, ...
// when built from a type list, as here), and duckdb_functions() reports those
// names, falling back to the description's only when the counts disagree —
// which they never do. So the documented names must be set on the signature
// to render as before. The parameters stay positional-only, so, as in v1.5, a
// caller still cannot pass them by name.
#ifdef SV_DUCKDB_2
template <class FUNCTION>
inline void name_positional_parameters_of(FUNCTION &function, const duckdb::vector<duckdb::string> &names) {
	auto &signature = function.GetSignature();
	const auto count = signature.GetPositionalParameterCount();
	for (duckdb::idx_t i = 0; i < count && i < names.size(); i++) {
		signature.GetParameter(i).SetName(duckdb::Identifier(names[i]));
	}
}
#endif

inline void name_positional_parameters(duckdb::TableFunction &function, const duckdb::vector<duckdb::string> &names) {
#ifdef SV_DUCKDB_2
	name_positional_parameters_of(function, names);
#else
	(void)function;
	(void)names;
#endif
}

inline void name_positional_parameters(duckdb::ScalarFunctionSet &set, const duckdb::vector<duckdb::string> &names) {
#ifdef SV_DUCKDB_2
	set.ApplyToFunctions([&](duckdb::ScalarFunction &function) { name_positional_parameters_of(function, names); });
#else
	(void)set;
	(void)names;
#endif
}

// Mark every overload of a scalar function as able to raise a runtime error.
//
// 2.0: a scalar function that throws must say so (`SetFallible()`), because
// the optimizer folds and reorders calls it believes cannot fail. One that
// throws without the mark is reported as an INTERNAL error ("threw an
// execution error, but the function is not marked as fallible") in place of
// its own message. Both of ours raise ordinary errors for an unknown or
// ambiguous view. v1.5 has no such marking, so this is a no-op there.
inline void mark_fallible(duckdb::ScalarFunctionSet &set) {
#ifdef SV_DUCKDB_2
	set.SetFallible();
#else
	(void)set;
#endif
}

} // namespace sv_compat
