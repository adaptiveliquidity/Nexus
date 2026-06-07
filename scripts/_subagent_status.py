"""Print status of subagent transcripts under the agent-transcripts dir."""
import json
import os
import sys

base = "/mnt/c/Users/Benna/.cursor/projects/c-Users-Benna-Documents-Nexus-Nexus/agent-transcripts/e78bb7af-4b4a-4c96-8036-160f62d43419/subagents"
for f in sorted(os.listdir(base)):
    if not f.endswith(".jsonl"):
        continue
    p = os.path.join(base, f)
    sz = os.path.getsize(p)
    last_type = "?"
    last_status = ""
    last_error = ""
    with open(p) as fh:
        lines = fh.readlines()
    if lines:
        try:
            d = json.loads(lines[-1])
            last_type = d.get("type", "?")
            last_status = d.get("status", "")
            last_error = (d.get("error") or "")[:80]
        except Exception:
            last_type = "(parse fail)"
    print(f"{f[:40]:<40} {sz:>6}B {len(lines):>3}l  {last_type:<15} {last_status:<10} {last_error}")
