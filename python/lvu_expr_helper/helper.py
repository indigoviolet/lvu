"""JSONL compiler for user-authored, row-local Polars expressions.

Expressions are trusted local code.  The AST checks enforce lvu's supported
expression subset; they are deliberately not represented as a security sandbox.
"""

from __future__ import annotations

import ast
import inspect
import io
import json
import sys
from dataclasses import dataclass
from typing import Any

import polars as pl

SCHEMA_VERSION = 1
MAX_REQUEST_BYTES = 64 * 1024
MAX_EXPRESSION_CHARS = 16 * 1024
EXPECTED_PYTHON_POLARS_VERSION = "1.44.1"
COMPATIBILITY_ID = f"polars-py-{EXPECTED_PYTHON_POLARS_VERSION}-rs-0.55.2-expr-json-v1"
# This gate controls construction side effects, not row-local semantics.  Rust
# asks the pinned Polars plan whether the constructed expression is elementwise.
_FORBIDDEN_CALLS = {
    "collect",
    "collect_async",
    "deserialize",
    "from_json",
    "inspect",
    "map_batches",
    "map_elements",
    "map_groups",
    "pipe",
    "register_plugin_function",
    "register_plugin",
    "serialize",
}
_FORBIDDEN_METHOD_PREFIXES = ("sink_", "write_")
_EXPR_NAMESPACES = {"arr", "bin", "cat", "dt", "list", "name", "str", "struct"}
_DTYPE_CONSTRUCTORS = {
    "Boolean", "Date", "Datetime", "Float32", "Float64", "Int8", "Int16",
    "Int32", "Int64", "String", "UInt8", "UInt16", "UInt32", "UInt64",
}


def _annotated_expr_constructors() -> frozenset[str]:
    constructors = set()
    for name, value in vars(pl).items():
        if name.startswith("_") or not callable(value):
            continue
        try:
            annotation = inspect.get_annotations(value, eval_str=False).get("return")
        except (TypeError, ValueError):
            continue
        if annotation == "Expr" or annotation is pl.Expr:
            constructors.add(name)
    # These pinned constructors have union/intermediate annotations but produce
    # expressions under the source shapes accepted below.
    constructors.update({"coalesce", "from_epoch", "struct", "when"})
    return frozenset(constructors)


_EXPR_CONSTRUCTORS = _annotated_expr_constructors() | {"col", "lit"}
_ALLOWED_NAMES = {
    "pl",
    "False",
    "None",
    "True",
}
@dataclass
class ProtocolError(Exception):
    code: str
    message: str


class _SubsetValidator(ast.NodeVisitor):
    _allowed_nodes = (
        ast.Expression,
        ast.Call,
        ast.Attribute,
        ast.Name,
        ast.Load,
        ast.Constant,
        ast.List,
        ast.Tuple,
        ast.Dict,
        ast.keyword,
        ast.BinOp,
        ast.BoolOp,
        ast.Compare,
        ast.UnaryOp,
        ast.Add,
        ast.Sub,
        ast.Mult,
        ast.Div,
        ast.FloorDiv,
        ast.Mod,
        ast.Pow,
        ast.BitAnd,
        ast.BitOr,
        ast.BitXor,
        ast.And,
        ast.Or,
        ast.Not,
        ast.Invert,
        ast.USub,
        ast.UAdd,
        ast.Eq,
        ast.NotEq,
        ast.Lt,
        ast.LtE,
        ast.Gt,
        ast.GtE,
    )

    def generic_visit(self, node: ast.AST) -> None:
        if not isinstance(node, self._allowed_nodes):
            raise ProtocolError(
                "unsupported_expression",
                f"{type(node).__name__} is outside the row-local expression subset",
            )
        super().generic_visit(node)

    def visit_Name(self, node: ast.Name) -> None:
        if node.id not in _ALLOWED_NAMES:
            raise ProtocolError(
                "unsupported_expression",
                f"name {node.id!r} is not available; use native pl expressions only",
            )

    def visit_Attribute(self, node: ast.Attribute) -> None:
        if node.attr.startswith("_"):
            raise ProtocolError("unsupported_expression", "private attributes are forbidden")
        if node.attr == "meta":
            raise ProtocolError(
                "unsupported_expression",
                "Expr.meta inspection and serialization APIs are unavailable",
            )
        self.generic_visit(node)

    @staticmethod
    def _direct_pl_call(node: ast.Call) -> bool:
        return isinstance(node.func, ast.Attribute) and isinstance(node.func.value, ast.Name) and node.func.value.id == "pl"

    @classmethod
    def _has_expr_provenance(cls, node: ast.AST) -> bool:
        if isinstance(node, ast.Call):
            if cls._direct_pl_call(node):
                return node.func.attr in _EXPR_CONSTRUCTORS
            return isinstance(node.func, ast.Attribute) and cls._has_expr_provenance(node.func.value)
        if isinstance(node, ast.Attribute):
            return cls._has_expr_provenance(node.value)
        return False

    def visit_Call(self, node: ast.Call) -> None:
        if not isinstance(node.func, ast.Attribute):
            raise ProtocolError("unsupported_expression", "only native Polars calls are allowed")
        if node.func.attr in _FORBIDDEN_CALLS or node.func.attr.startswith(_FORBIDDEN_METHOD_PREFIXES):
            raise ProtocolError(
                "callback_forbidden",
                f"{node.func.attr} callbacks, eager actions, or I/O are unavailable",
            )
        if isinstance(node.func.value, ast.Name) and node.func.value.id == "pl":
            if node.func.attr not in _EXPR_CONSTRUCTORS | _DTYPE_CONSTRUCTORS:
                raise ProtocolError(
                    "unsupported_expression",
                    f"pl.{node.func.attr} is not a pinned expression constructor",
                )
            eager = next((keyword.value for keyword in node.keywords if keyword.arg == "eager"), None)
            if node.func.attr in {"coalesce", "struct"} and eager is not None and not (
                isinstance(eager, ast.Constant) and eager.value is False
            ):
                raise ProtocolError("unsupported_expression", "eager expression construction is unavailable")
            if node.func.attr == "from_epoch":
                column = node.args[0] if node.args else next(
                    (keyword.value for keyword in node.keywords if keyword.arg == "column"), None
                )
                if column is None or not (
                    self._has_expr_provenance(column)
                    or isinstance(column, ast.Constant) and isinstance(column.value, str)
                ):
                    raise ProtocolError(
                        "unsupported_expression",
                        "eager from_epoch Series construction is unavailable; use a column name or expression",
                    )
        elif not self._has_expr_provenance(node.func.value):
            raise ProtocolError(
                "unsupported_expression",
                "calls through nested Polars namespaces or unknown objects are unavailable",
            )
        elif isinstance(node.func.value, ast.Attribute) and node.func.value.attr not in _EXPR_NAMESPACES:
            raise ProtocolError(
                "unsupported_expression",
                f"{node.func.value.attr!r} is not a documented expression transformation namespace",
            )
        if node.func.attr in {"strptime", "to_date", "to_datetime"}:
            # str.strptime(dtype, format, ...) differs from to_date/to_datetime,
            # whose first positional argument is the format.
            position = 1 if node.func.attr == "strptime" else 0
            format_arg = node.args[position] if len(node.args) > position else next(
                (keyword.value for keyword in node.keywords if keyword.arg == "format"), None
            )
            if format_arg is None or (isinstance(format_arg, ast.Constant) and format_arg.value is None):
                raise ProtocolError(
                    "explicit_datetime_format_required",
                    f"{node.func.attr} requires an explicit format in live mode; format inference varies by batch",
                )
        for arg in [*node.args, *(keyword.value for keyword in node.keywords)]:
            if isinstance(arg, (ast.Lambda, ast.Name)) or (
                isinstance(arg, ast.Attribute)
                and not (
                    isinstance(arg.value, ast.Name)
                    and arg.value.id == "pl"
                    and arg.attr in _DTYPE_CONSTRUCTORS
                )
            ):
                raise ProtocolError("callback_forbidden", "callbacks and callable arguments are forbidden")
        self.generic_visit(node)


def compile_expression(source: str, kind: str) -> str:
    if len(source) > MAX_EXPRESSION_CHARS:
        raise ProtocolError("expression_too_large", f"expression exceeds {MAX_EXPRESSION_CHARS} characters")
    try:
        tree = ast.parse(source, mode="eval")
    except SyntaxError as exc:
        raise ProtocolError("syntax_error", f"line {exc.lineno}, column {exc.offset}: {exc.msg}") from exc
    _SubsetValidator().visit(tree)
    try:
        expression = eval(compile(tree, "<lvu-expression>", "eval"), {"__builtins__": {}, "pl": pl}, {})
    except Exception as exc:
        raise ProtocolError("compile_error", f"{type(exc).__name__}: {exc}") from exc
    if not isinstance(expression, pl.Expr):
        raise ProtocolError("not_an_expression", "definition must evaluate to one polars.Expr")
    if expression.meta.has_multiple_outputs():
        raise ProtocolError(
            "multiple_outputs", "column selectors and multi-output expressions are not supported"
        )
    try:
        output_name = expression.meta.output_name()
    except Exception as exc:
        raise ProtocolError("unknown_output", f"expression output cannot be determined: {exc}") from exc
    if kind == "enrichment" and output_name.startswith("_lvu_"):
        raise ProtocolError("protected_column", f"expression cannot write protected column {output_name!r}")
    if kind == "filter":
        # The concrete Boolean result is checked by the host against its schema.
        # This catches obvious scalar definitions without requiring sample data.
        if expression.meta.is_literal():
            raise ProtocolError("invalid_filter", "a filter must depend on row columns")
    serialized = expression.meta.serialize(format="json")
    # Deserialize once in Python to catch corrupt/internally incompatible output.
    # Rust performs the authoritative typed structural validation of this cache.
    pl.Expr.deserialize(io.StringIO(serialized), format="json")
    return serialized


def handle(request: Any) -> dict[str, Any]:
    request_id = request.get("request_id") if isinstance(request, dict) else None
    try:
        if not isinstance(request, dict):
            raise ProtocolError("invalid_request", "request must be a JSON object")
        request_id = request.get("request_id")
        if not isinstance(request_id, str) or not request_id or len(request_id) > 256:
            request_id = None
            raise ProtocolError("invalid_request_id", "request_id must be a non-empty string of at most 256 characters")
        schema_version = request.get("schema_version")
        if not isinstance(schema_version, int) or isinstance(schema_version, bool) or schema_version != SCHEMA_VERSION:
            raise ProtocolError("unsupported_schema", "schema_version must be 1")
        operation = request.get("operation")
        if not isinstance(operation, str) or operation != "compile":
            raise ProtocolError("unsupported_operation", "operation must be 'compile'")
        kind = request.get("kind")
        if not isinstance(kind, str) or kind not in {"filter", "enrichment", "color"}:
            raise ProtocolError("invalid_kind", "kind must be filter, enrichment, or color")
        source = request.get("expression")
        if not isinstance(source, str):
            raise ProtocolError("invalid_expression", "expression must be a string")
        if pl.__version__ != EXPECTED_PYTHON_POLARS_VERSION:
            raise ProtocolError(
                "incompatible_python_polars",
                f"helper requires Python Polars {EXPECTED_PYTHON_POLARS_VERSION}, found {pl.__version__}",
            )
        expression_json = compile_expression(source, kind)
        return {
            "request_id": request_id,
            "schema_version": SCHEMA_VERSION,
            "ok": True,
            "expression_json": expression_json,
            "python_polars_version": pl.__version__,
            "compatibility_id": COMPATIBILITY_ID,
        }
    except ProtocolError as exc:
        return {"schema_version": SCHEMA_VERSION, "request_id": request_id, "ok": False, "error": {"code": exc.code, "message": exc.message}}
    except Exception as exc:
        return {"schema_version": SCHEMA_VERSION, "request_id": request_id, "ok": False, "error": {"code": "compile_error", "message": f"{type(exc).__name__}: {exc}"}}


def main() -> int:
    while raw := sys.stdin.buffer.readline(MAX_REQUEST_BYTES + 2):
        if len(raw) > MAX_REQUEST_BYTES or not raw.endswith(b"\n"):
            response = {"schema_version": SCHEMA_VERSION, "request_id": None, "ok": False, "error": {"code": "request_too_large", "message": f"request line exceeds {MAX_REQUEST_BYTES} bytes"}}
            # Discard the remainder of an oversized line before resynchronizing.
            while raw and not raw.endswith(b"\n"):
                raw = sys.stdin.buffer.readline(MAX_REQUEST_BYTES + 2)
        else:
            try:
                response = handle(json.loads(raw))
            except (json.JSONDecodeError, UnicodeDecodeError, RecursionError) as exc:
                response = {"schema_version": SCHEMA_VERSION, "request_id": None, "ok": False, "error": {"code": "invalid_json", "message": str(exc)}}
        sys.stdout.write(json.dumps(response, separators=(",", ":")) + "\n")
        sys.stdout.flush()
    return 0
