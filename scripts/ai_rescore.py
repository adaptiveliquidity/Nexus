"""CI helper that calls a real LLM to score the Phase 3 ErrorLog index.

Mirrors the prompt the in-IDE rescore subagents use. Picks the first
available provider (Anthropic if `ANTHROPIC_API_KEY` is set, otherwise
OpenAI). The output is markdown matching the same structure as the
human-reviewer rescore files so the analyzer can pick it up unchanged.

This script lives in `scripts/` so the GitHub Actions workflow doesn't
need to embed a multi-line prompt in YAML.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys
import textwrap
from datetime import datetime, timezone

ACCURACY_RE = re.compile(r"Aggregate accuracy rate:\s*([0-9]+(?:\.[0-9]+)?)\s*%")


def build_prompt(index: list) -> str:
    blob = json.dumps(index, indent=2)
    return textwrap.dedent(
        f"""\
        You are an expert systems reliability engineer scoring the Nexus
        WASM sandbox Phase 3 AI Telemetry Validation. Given the captured
        ErrorLog index below, score the per-scenario recovery actions
        using the rubric:
          1. Each action technically correct for the observed failure mode?
          2. Optimal (minimal blast radius, fastest recovery, preserves state)?
          3. Better alternatives or missing steps?
          4. Soundness score 1-10 with justification.
          5. Comment on `trigger_status`, `rollback_performed`,
             `failure_mode` precision.

        Reply in markdown using exactly this template (the analyzer
        parses these anchors: `Score: X/10`, `Average score: X.X`,
        `Aggregate accuracy rate: X%`):

            # Phase 3 AI Telemetry Validation -- CI rescore
            Model: <model id>
            Date (UTC): {datetime.now(timezone.utc).isoformat(timespec='seconds')}
            Input: artifacts/raw/phase3_index.json

            ## Per-scenario analysis
            ### 1. <scenario>
            ...
            Score: X/10
            ...

            ## Overall verdict
            - Aggregate accuracy rate: X%
            - Average score: X.X / 10
            - Remaining defects: ...

        ErrorLog index:

        ```json
        {blob}
        ```
        """
    )


def call_anthropic(prompt: str) -> tuple[str, str]:
    import anthropic
    client = anthropic.Anthropic()
    model = os.environ.get("NEXUS_RESCORE_MODEL", "claude-sonnet-4-6")
    resp = client.messages.create(
        model=model,
        max_tokens=4096,
        messages=[{"role": "user", "content": prompt}],
    )
    text = "".join(b.text for b in resp.content if hasattr(b, "text"))
    return model, text


def call_openai(prompt: str) -> tuple[str, str]:
    from openai import OpenAI
    client = OpenAI()
    model = os.environ.get("NEXUS_RESCORE_MODEL", "gpt-4o-mini")
    resp = client.chat.completions.create(
        model=model,
        messages=[
            {"role": "system", "content": "Score the Nexus telemetry honestly per the rubric."},
            {"role": "user", "content": prompt},
        ],
        max_tokens=4096,
    )
    return model, resp.choices[0].message.content or ""


def positive_int(value: str) -> int:
    try:
        parsed = int(value)
    except ValueError as exc:
        raise argparse.ArgumentTypeError("--samples must be an integer") from exc
    if parsed < 1:
        raise argparse.ArgumentTypeError("--samples must be >= 1")
    return parsed


def parse_accuracy(text: str) -> float:
    match = ACCURACY_RE.search(text)
    if not match:
        raise ValueError("could not parse Aggregate accuracy rate from LLM response")
    return float(match.group(1))


def score_once(prompt: str) -> tuple[str, str]:
    if os.environ.get("ANTHROPIC_API_KEY"):
        return call_anthropic(prompt)
    if os.environ.get("OPENAI_API_KEY"):
        return call_openai(prompt)
    raise RuntimeError("no provider env var set (ANTHROPIC_API_KEY or OPENAI_API_KEY)")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--index", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--samples", type=positive_int, default=3)
    args = ap.parse_args()

    with open(args.index) as f:
        index = json.load(f)
    prompt = build_prompt(index)

    scored = []
    try:
        for _ in range(args.samples):
            model, text = score_once(prompt)
            scored.append((parse_accuracy(text), model, text))
    except (RuntimeError, ValueError) as exc:
        print(str(exc), file=sys.stderr)
        return 2

    _, model, text = sorted(scored, key=lambda sample: sample[0])[len(scored) // 2]

    with open(args.out, "w") as f:
        f.write(f"<!-- model: {model} -->\n{text}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
