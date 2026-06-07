"""Rebuild artifacts/raw/phase3_index.json with trimmed strings."""
import json
import os
import glob

THIS = os.path.dirname(os.path.abspath(__file__))
NEXUS_ROOT = os.path.abspath(os.path.join(THIS, ".."))
RAW_DIR = os.path.join(NEXUS_ROOT, "artifacts", "raw")

MAX = 1500


def trim(s):
    if not isinstance(s, str):
        return s
    if len(s) <= MAX:
        return s
    return s[:MAX] + f"\n...[truncated {len(s)-MAX} chars; see raw file]"


def main():
    out = []
    for p in sorted(glob.glob(os.path.join(RAW_DIR, "phase3_*.json"))):
        base = os.path.basename(p)
        if base in ("phase3_index.json", "phase3_ai_validation.json"):
            continue
        with open(p) as f:
            d = json.load(f)
        el = d.get("error_log") or {}
        tool = d.get("tool_output") or {}
        out.append(
            {
                "scenario": d.get("scenario"),
                "path": os.path.relpath(p, NEXUS_ROOT),
                "success": tool.get("success"),
                "rollback_performed": tool.get("rollback_performed"),
                "execution_time_ms": tool.get("execution_time_ms"),
                "fuel_consumed": tool.get("fuel_consumed"),
                # Typed failure classification (Phase A). String-or-dict
                # depending on whether the variant carries data.
                "failure_mode": el.get("failure_mode"),
                "error_type": trim(el.get("error_type")),
                "description": trim(el.get("description")),
                "trigger_status": el.get("trigger_status"),
                # Structured recovery actions (Phase A). Each entry has
                # description / confidence / source / non_retryable.
                "recovery_actions": el.get("recovery_actions"),
                "tool_output_error": trim(tool.get("error")),
            }
        )
    out_path = os.path.join(RAW_DIR, "phase3_index.json")
    with open(out_path, "w") as f:
        json.dump(out, f, indent=2)
    print(f"wrote {out_path} size={os.path.getsize(out_path)} scenarios={len(out)}")


if __name__ == "__main__":
    main()
