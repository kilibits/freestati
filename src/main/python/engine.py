"""
FreeStats Python statistical engine.

Transport: 4-byte big-endian length prefix + msgpack payload (both directions).
Concurrency: asyncio event loop + ThreadPoolExecutor for CPU-bound Polars work.
Hot path (get_page): Polars write_json(row_oriented=True) — zero Python dict
  creation; rows are serialized directly from Rust and returned as a raw JSON
  string field (`rows_raw`) that Node.js parses with V8's native JSON.parse.
"""

import sys
import os
import asyncio
import threading
import traceback
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

def _log(msg: str) -> None:
    print(f"[Python] {msg}", file=sys.stderr, flush=True)

try:
    import polars as pl
    import msgpack
    import pandas as pd
    import numpy as np
except ImportError as e:
    _log(f"CRITICAL: Missing dependency: {e}")
    _log("Please run: pip install polars msgpack pandas numpy pyreadstat openpyxl")
    sys.exit(1)

# ── Global state ──────────────────────────────────────────────────────────────

_df: pl.DataFrame | None = None
_var_meta: dict[str, dict] = {}
_current_path: str | None = None
_cached_variables: list[dict] | None = None

# Protects mutable state (_df, _var_meta) during write operations.
# Read-only ops (get_page, get_variables) need no lock — Polars slices are safe.
_data_lock = threading.RLock()

# Thread pool for CPU-bound Polars work (releases GIL between ops)
_executor = ThreadPoolExecutor(max_workers=4, thread_name_prefix="fs-worker")

# Stdout write lock — multiple worker threads must not interleave bytes
_write_lock = threading.Lock()


# ── Transport ─────────────────────────────────────────────────────────────────

def _send(obj: dict) -> None:
    """Pack obj as msgpack and write a length-prefixed frame to stdout."""
    try:
        payload = msgpack.packb(obj, use_bin_type=True)
        header = len(payload).to_bytes(4, "big")
        with _write_lock:
            sys.stdout.buffer.write(header + payload)
            sys.stdout.buffer.flush()
    except Exception as exc:
        _log(f"Serialization error: {exc}")


def _send_raw_rows(msg_id: str, rows_json: str, total: int) -> None:
    """
    Fast path for get_page: rows_json is already a UTF-8 JSON string produced
    by Polars in Rust.  We pack the envelope with msgpack but embed the rows
    payload as a plain string field ('rows_raw') so Node.js can JSON.parse it
    with V8's native parser — no Python dict objects are ever created.
    """
    payload = msgpack.packb(
        {"id": msg_id, "result": {"rows_raw": rows_json, "total": total}},
        use_bin_type=True,
    )
    header = len(payload).to_bytes(4, "big")
    with _write_lock:
        sys.stdout.buffer.write(header + payload)
        sys.stdout.buffer.flush()


# ── Helpers ───────────────────────────────────────────────────────────────────

def _infer_measure(series: pl.Series) -> str:
    if series.dtype.is_numeric():
        try:
            return "ordinal" if series.n_unique() <= 10 else "scale"
        except Exception:
            return "scale"
    return "nominal"


def _infer_decimals(series: pl.Series) -> int:
    if not series.dtype.is_numeric():
        return 0
    try:
        if series.null_count() == len(series):
            return 0
        return 0 if (series.drop_nulls() == series.drop_nulls().floor()).all() else 2
    except Exception:
        return 2


def _series_type(dtype: pl.DataType) -> str:
    if dtype.is_temporal():
        return "date"
    if dtype.is_numeric():
        return "numeric"
    return "string"


def _variables_payload() -> list[dict]:
    global _cached_variables
    if _df is None:
        return []
    if _cached_variables is not None:
        return _cached_variables
    out = []
    for col in _df.columns:
        m = _var_meta.get(col, {})
        dtype = _df[col].dtype
        v_type = m.get("type", _series_type(dtype))
        out.append({
            "name": col,
            "label": m.get("label", ""),
            "type": v_type,
            "width": m.get("width", 8),
            "decimals": m.get("decimals", _infer_decimals(_df[col]) if v_type == "numeric" else 0),
            "columns": m.get("columns", 8),
            "align": m.get("align", "left"),
            "valueLabels": m.get("valueLabels", {}),
            "missingValues": m.get("missingValues", []),
            "measureLevel": m.get("measureLevel", _infer_measure(_df[col])),
            "role": m.get("role", "input"),
        })
    _cached_variables = out
    return out


def _sanitise_floats(frame: pl.DataFrame) -> pl.DataFrame:
    """Replace NaN/Inf with null so Polars write_json produces valid JSON."""
    float_cols = [c for c in frame.columns if frame.schema[c] in (pl.Float32, pl.Float64)]
    if not float_cols:
        return frame
    return frame.with_columns([
        pl.when(pl.col(c).is_nan() | pl.col(c).is_infinite())
        .then(None)
        .otherwise(pl.col(c))
        .alias(c)
        for c in float_cols
    ])


# ── Command handlers ──────────────────────────────────────────────────────────

def cmd_load_file(args: dict) -> dict:
    global _df, _var_meta, _current_path, _cached_variables
    file_path: str = args["path"]
    ext = Path(file_path).suffix.lower()
    _log(f"Loading: {file_path}")

    with _data_lock:
        _df = None
        _var_meta = {}
        _cached_variables = None

        try:
            if ext in (".tab", ".tsv"):
                _df = pl.read_csv(file_path, separator="\t", ignore_errors=True)
            elif ext == ".csv":
                _df = pl.read_csv(file_path, ignore_errors=True)
            elif ext in (".xlsx", ".xls"):
                _df = pl.read_excel(file_path)
            elif ext in (".sav", ".dta", ".sas7bdat"):
                import pyreadstat  # type: ignore
                if ext == ".sav":
                    pdf, meta = pyreadstat.read_sav(file_path)
                    for col in pdf.columns:
                        idx = meta.column_names.index(col) if col in meta.column_names else -1
                        if idx >= 0:
                            missing = list(meta.missing_user_values.get(col, []))
                            _var_meta[col] = {
                                "label": meta.column_labels[idx],
                                "valueLabels": {str(k): v for k, v in meta.variable_value_labels.get(col, {}).items()},
                                "measureLevel": {1: "nominal", 2: "ordinal", 3: "scale"}.get(
                                    meta.variable_measure.get(col, 3), "scale"),
                                "columns": meta.variable_display_width.get(col, 8),
                                "align": meta.variable_alignment.get(col, "left").lower(),
                                "missingValues": missing,
                            }
                elif ext == ".dta":
                    pdf, _ = pyreadstat.read_dta(file_path)
                else:
                    pdf, _ = pyreadstat.read_sas7bdat(file_path)
                if ext != ".sav":
                    _df = pl.from_pandas(pdf)
                else:
                    _df = pl.from_pandas(pdf)
            else:
                return {"error": f"Unsupported file type: {ext}"}
        except Exception as exc:
            return {"error": f"Load failed: {exc}", "traceback": traceback.format_exc()}

        if _df is None:
            return {"error": "Failed to load dataset"}

        _current_path = file_path
        _log(f"Loaded {len(_df)} rows × {len(_df.columns)} cols")

        return {
            "rowCount": len(_df),
            "colCount": len(_df.columns),
            "variables": _variables_payload(),
            "filename": os.path.basename(file_path),
            "path": file_path,
        }


# Sentinel: handler sent the response directly; dispatch should skip _send
_SENT = object()

def cmd_get_page(args: dict) -> object:
    """
    Hot path. Uses Polars' Rust JSON writer instead of to_dicts() to avoid
    creating Python dict objects.  The response is sent directly from this
    function via _send_raw_rows; dispatch checks for _SENT and skips _send.
    """
    if _df is None:
        _send_raw_rows(args.get("__msg_id__", "?"), "[]", 0)
        return _SENT

    offset = int(args.get("offset", 0))
    limit  = int(args.get("limit", 500))
    total  = len(_df)

    if offset >= total:
        _send_raw_rows(args.get("__msg_id__", "?"), "[]", total)
        return _SENT

    chunk = _df.slice(offset, min(limit, total - offset))

    # Add 1-based case number column
    chunk = chunk.with_columns(
        (pl.lit(offset + 1) + pl.int_range(0, pl.len())).alias("__row__")
    )

    # Replace NaN/Inf so Polars produces valid JSON
    chunk = _sanitise_floats(chunk)

    # Polars serialises directly in Rust — zero Python dict allocation
    rows_json = chunk.write_json(row_oriented=True)

    _send_raw_rows(args.get("__msg_id__", "?"), rows_json, total)
    return _SENT


def cmd_get_variables(_args: dict) -> dict:
    return {"variables": _variables_payload()}


def cmd_set_variable_meta(args: dict) -> dict:
    global _cached_variables
    with _data_lock:
        _var_meta.setdefault(args["varName"], {}).update(args["meta"])
        _cached_variables = None
    return {"ok": True}


def cmd_update_cell(args: dict) -> dict:
    global _df
    with _data_lock:
        if _df is None:
            return {"error": "No dataset loaded"}
        try:
            _df[int(args["row"]) - 1, args["col"]] = args["value"]
            return {"ok": True}
        except Exception as exc:
            return {"error": str(exc)}


def cmd_save_file(args: dict) -> dict:
    with _data_lock:
        if _df is None:
            return {"error": "No dataset loaded"}
        file_path: str = args["path"]
        ext = Path(file_path).suffix.lower()
        try:
            if ext in (".tab", ".tsv"):
                _df.write_csv(file_path, separator="\t")
            elif ext == ".csv":
                _df.write_csv(file_path)
            elif ext == ".xlsx":
                _df.write_excel(file_path)
            elif ext == ".sav":
                import pyreadstat  # type: ignore
                pyreadstat.write_sav(_df.to_pandas(), file_path)
            else:
                _df.write_csv(file_path)
            return {"ok": True, "path": file_path}
        except Exception as exc:
            return {"error": str(exc)}


def cmd_new_dataset(_args: dict) -> dict:
    global _df, _var_meta, _current_path, _cached_variables
    with _data_lock:
        _df = pl.DataFrame()
        _var_meta = {}
        _current_path = None
        _cached_variables = None
    return {"ok": True, "rowCount": 0, "colCount": 0, "variables": []}


COMMANDS: dict[str, object] = {
    "load_file":         cmd_load_file,
    "get_page":          cmd_get_page,
    "get_variables":     cmd_get_variables,
    "set_variable_meta": cmd_set_variable_meta,
    "update_cell":       cmd_update_cell,
    "save_file":         cmd_save_file,
    "new_dataset":       cmd_new_dataset,
}


# ── Async dispatch ────────────────────────────────────────────────────────────

async def dispatch(msg: dict, loop: asyncio.AbstractEventLoop) -> None:
    msg_id = msg.get("id", "?")
    cmd_type: str = msg.get("type", "")
    handler = COMMANDS.get(cmd_type)

    if handler is None:
        _send({"id": msg_id, "error": f"Unknown command: {cmd_type!r}"})
        return

    args: dict = dict(msg.get("args") or {})
    # Inject message ID so get_page can send its own response
    if cmd_type == "get_page":
        args["__msg_id__"] = msg_id

    try:
        result = await loop.run_in_executor(_executor, handler, args)
        if result is not _SENT:
            _send({"id": msg_id, "result": result})
    except Exception as exc:
        _send({"id": msg_id, "error": str(exc), "traceback": traceback.format_exc()})


# ── Main loop — binary stdin reader + asyncio ─────────────────────────────────

async def main_async() -> None:
    _send({"type": "ready"})
    loop = asyncio.get_event_loop()
    queue: asyncio.Queue[dict | None] = asyncio.Queue()

    def stdin_reader() -> None:
        """Blocking binary reader in a background thread."""
        stdin_bin = sys.stdin.buffer
        while True:
            header = stdin_bin.read(4)
            if len(header) < 4:
                break
            length = int.from_bytes(header, "big")
            data = stdin_bin.read(length)
            if len(data) < length:
                break
            try:
                msg = msgpack.unpackb(data, raw=False)
                loop.call_soon_threadsafe(queue.put_nowait, msg)
            except Exception as exc:
                _log(f"Decode error: {exc}")
        loop.call_soon_threadsafe(queue.put_nowait, None)

    reader_thread = threading.Thread(target=stdin_reader, daemon=True, name="fs-stdin")
    reader_thread.start()

    while True:
        msg = await queue.get()
        if msg is None:
            break
        asyncio.create_task(dispatch(msg, loop))


def main() -> None:
    asyncio.run(main_async())


if __name__ == "__main__":
    main()
