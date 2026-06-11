"""
FreeStats Python statistical engine.
Communicates with the Electron main process via newline-delimited JSON on
stdin/stdout.  Protocol:
  request:  {"id": "<uuid>", "type": "<command>", "args": {...}}
  response: {"id": "<uuid>", "result": {...}}
         or {"id": "<uuid>", "error": "<message>"}
  startup:  {"type": "ready"}            (sent once on boot)
"""

import sys
import json
import os
import traceback
from pathlib import Path

import pandas as pd
import numpy as np

# ── Global state ──────────────────────────────────────────────────────────────

_df: pd.DataFrame | None = None
_var_meta: dict[str, dict] = {}   # per-column metadata overrides
_current_path: str | None = None


# ── Helpers ───────────────────────────────────────────────────────────────────

def _send(obj: dict) -> None:
    print(json.dumps(obj, default=str), flush=True)


def _infer_measure(series: pd.Series) -> str:
    if pd.api.types.is_numeric_dtype(series):
        return "ordinal" if series.nunique() <= 10 else "scale"
    return "nominal"


def _series_type(series: pd.Series) -> str:
    if pd.api.types.is_datetime64_any_dtype(series):
        return "date"
    if pd.api.types.is_numeric_dtype(series):
        return "numeric"
    return "string"


def _safe_value(v):
    """Convert numpy scalars / NaN / Inf to JSON-serialisable Python types."""
    if isinstance(v, float) and (np.isnan(v) or np.isinf(v)):
        return None
    if isinstance(v, (np.integer,)):
        return int(v)
    if isinstance(v, (np.floating,)):
        return float(v)
    if isinstance(v, (np.bool_,)):
        return bool(v)
    return v


def _variables_payload() -> list[dict]:
    if _df is None:
        return []
    out = []
    for col in _df.columns:
        m = _var_meta.get(col, {})
        out.append({
            "name": col,
            "label": m.get("label", ""),
            "type": m.get("type", _series_type(_df[col])),
            "width": m.get("width", 8),
            "decimals": m.get("decimals", 2 if _series_type(_df[col]) == "numeric" else 0),
            "valueLabels": m.get("valueLabels", {}),
            "missingValues": m.get("missingValues", []),
            "measureLevel": m.get("measureLevel", _infer_measure(_df[col])),
            "role": m.get("role", "input"),
        })
    return out


# ── Command handlers ──────────────────────────────────────────────────────────

def cmd_load_file(args: dict) -> dict:
    global _df, _var_meta, _current_path
    file_path: str = args["path"]
    ext = Path(file_path).suffix.lower()

    if ext in (".tab", ".tsv"):
        _df = pd.read_csv(file_path, sep="\t", encoding_errors="replace", low_memory=False)
    elif ext == ".csv":
        _df = pd.read_csv(file_path, encoding_errors="replace", low_memory=False)
    elif ext in (".xlsx", ".xls"):
        _df = pd.read_excel(file_path)
    elif ext == ".sav":
        try:
            import pyreadstat  # type: ignore
            _df, meta = pyreadstat.read_sav(file_path)
            for col in _df.columns:
                idx = meta.column_names.index(col) if col in meta.column_names else -1
                _var_meta[col] = {
                    "label": meta.column_labels[idx] if idx >= 0 else "",
                    "valueLabels": {
                        str(k): v
                        for k, v in meta.variable_value_labels.get(col, {}).items()
                    },
                    "measureLevel": {1: "nominal", 2: "ordinal", 3: "scale"}.get(
                        meta.variable_measure.get(col, 3), "scale"
                    ),
                }
        except ImportError:
            return {"error": "pyreadstat is not installed. Run: pip install pyreadstat"}
    elif ext == ".dta":
        _df = pd.read_stata(file_path)
    elif ext == ".sas7bdat":
        try:
            import pyreadstat  # type: ignore
            _df, _ = pyreadstat.read_sas7bdat(file_path)
        except ImportError:
            return {"error": "pyreadstat is not installed. Run: pip install pyreadstat"}
    else:
        return {"error": f"Unsupported file type: {ext}"}

    # Normalise column names to strings
    _df.columns = [str(c) for c in _df.columns]
    _current_path = file_path

    return {
        "rowCount": len(_df),
        "colCount": len(_df.columns),
        "variables": _variables_payload(),
        "filename": os.path.basename(file_path),
        "path": file_path,
    }


def cmd_get_page(args: dict) -> dict:
    if _df is None:
        return {"rows": [], "total": 0}
    offset = int(args.get("offset", 0))
    limit = int(args.get("limit", 100))
    chunk = _df.iloc[offset: offset + limit]
    rows = []
    for i, record in enumerate(chunk.to_dict("records")):
        row: dict = {"__row__": offset + i + 1}  # 1-based case number
        for k, v in record.items():
            row[k] = _safe_value(v)
        rows.append(row)
    return {"rows": rows, "total": len(_df)}


def cmd_get_variables(_args: dict) -> dict:
    return {"variables": _variables_payload()}


def cmd_set_variable_meta(args: dict) -> dict:
    var_name: str = args["varName"]
    meta: dict = args["meta"]
    _var_meta.setdefault(var_name, {}).update(meta)
    return {"ok": True}


def cmd_update_cell(args: dict) -> dict:
    if _df is None:
        return {"error": "No dataset loaded"}
    row = int(args["row"]) - 1  # convert 1-based case number back to 0-based
    col: str = args["col"]
    value = args["value"]
    _df.at[row, col] = value
    return {"ok": True}


def cmd_save_file(args: dict) -> dict:
    if _df is None:
        return {"error": "No dataset loaded"}
    file_path: str = args["path"]
    ext = Path(file_path).suffix.lower()
    if ext in (".tab", ".tsv"):
        _df.to_csv(file_path, sep="\t", index=False)
    elif ext == ".csv":
        _df.to_csv(file_path, index=False)
    elif ext in (".xlsx",):
        _df.to_excel(file_path, index=False, engine="openpyxl")
    elif ext == ".sav":
        try:
            import pyreadstat  # type: ignore
            pyreadstat.write_sav(_df, file_path)
        except ImportError:
            return {"error": "pyreadstat is not installed"}
    else:
        _df.to_csv(file_path, index=False)
    return {"ok": True, "path": file_path}


def cmd_new_dataset(_args: dict) -> dict:
    global _df, _var_meta, _current_path
    _df = pd.DataFrame()
    _var_meta = {}
    _current_path = None
    return {"ok": True, "rowCount": 0, "colCount": 0, "variables": []}


# ── Dispatch table ────────────────────────────────────────────────────────────

COMMANDS = {
    "load_file": cmd_load_file,
    "get_page": cmd_get_page,
    "get_variables": cmd_get_variables,
    "set_variable_meta": cmd_set_variable_meta,
    "update_cell": cmd_update_cell,
    "save_file": cmd_save_file,
    "new_dataset": cmd_new_dataset,
}


# ── Main loop ─────────────────────────────────────────────────────────────────

def main() -> None:
    _send({"type": "ready"})
    for raw_line in sys.stdin:
        line = raw_line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
            cmd_type: str = msg.get("type", "")
            handler = COMMANDS.get(cmd_type)
            if handler:
                result = handler(msg.get("args", {}))
                _send({"id": msg["id"], "result": result})
            else:
                _send({"id": msg["id"], "error": f"Unknown command: {cmd_type!r}"})
        except Exception as exc:  # noqa: BLE001
            _send({
                "id": msg.get("id", "?"),
                "error": str(exc),
                "traceback": traceback.format_exc(),
            })


if __name__ == "__main__":
    main()
