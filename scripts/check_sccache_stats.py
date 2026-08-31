#!/usr/bin/env python3
"""Report machine-readable sccache statistics and optionally require compiler requests."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


class StatsError(RuntimeError):
    pass


def _integer(value: object, label: str) -> int:
    if type(value) is not int or value < 0:
        raise StatsError(f"{label} must be a non-negative integer")
    return value


def _optional_integer(value: object, label: str) -> int | None:
    return None if value is None else _integer(value, label)


def _sum_integers(value: object) -> int:
    if type(value) is int:
        return max(value, 0)
    if isinstance(value, dict):
        return sum(_sum_integers(item) for item in value.values())
    return 0


def _language_count(metric: object, language: str) -> int:
    if not isinstance(metric, dict):
        raise StatsError("per-language statistic must be an object")
    counts = metric.get("counts")
    if not isinstance(counts, dict):
        raise StatsError("per-language statistic is missing counts")
    if language in counts:
        return _integer(counts[language], f"{language} count")
    advanced = metric.get("adv_counts", {})
    return _sum_integers(advanced.get(language, {})) if isinstance(advanced, dict) else 0


def summarize(
    stats_document: object, minimum_rust_hit_rate: float | None = None
) -> dict[str, Any]:
    if not isinstance(stats_document, dict) or not isinstance(stats_document.get("stats"), dict):
        raise StatsError("sccache JSON must contain a stats object")
    stats = stats_document["stats"]
    compile_requests = _integer(stats.get("compile_requests"), "compile_requests")
    hits = _language_count(stats.get("cache_hits"), "Rust")
    misses = _language_count(stats.get("cache_misses"), "Rust")
    errors = _language_count(stats.get("cache_errors"), "Rust")
    read_errors = _integer(stats.get("cache_read_errors"), "cache_read_errors")
    write_errors = _integer(stats.get("cache_write_errors"), "cache_write_errors")
    cache_size = _optional_integer(stats_document.get("cache_size"), "cache_size")
    max_cache_size = _optional_integer(
        stats_document.get("max_cache_size"), "max_cache_size"
    )
    rust_requests = hits + misses + errors
    hit_rate = hits * 100.0 / (hits + misses) if hits + misses else 0.0
    cache_utilization = (
        cache_size * 100.0 / max_cache_size
        if cache_size is not None and max_cache_size
        else None
    )
    warnings = []
    if compile_requests == 0 or rust_requests == 0:
        warnings.append("Rust build recorded zero sccache compile requests")
    if read_errors or write_errors or errors:
        warnings.append("sccache recorded Rust or cache read/write errors")
    if minimum_rust_hit_rate is not None and hit_rate < minimum_rust_hit_rate:
        warnings.append(
            f"Rust cache hit rate {hit_rate:.2f}% is below {minimum_rust_hit_rate:.2f}%"
        )
    if cache_utilization is not None and cache_utilization >= 95:
        warnings.append(
            "sccache is near capacity; eviction or profile thrashing may occur"
        )
    return {
        "schema": 1,
        "result": "reported",
        "compile_requests": compile_requests,
        "rust_requests": rust_requests,
        "rust_hits": hits,
        "rust_misses": misses,
        "rust_hit_rate": round(hit_rate, 2),
        "cache_size_bytes": cache_size,
        "max_cache_size_bytes": max_cache_size,
        "cache_utilization": (
            round(cache_utilization, 2) if cache_utilization is not None else None
        ),
        "cache_read_errors": read_errors,
        "cache_write_errors": write_errors,
        "warnings": warnings,
    }


def _gib(value: int) -> str:
    return f"{value / (1024**3):.2f} GiB"


def append_github_summary(path: Path, profile: str, result: dict[str, Any]) -> None:
    lines = [f"### sccache: {profile}", ""]
    if result["result"] == "reported":
        cache_size = (
            "unavailable"
            if result["cache_utilization"] is None
            else (
                f'{_gib(result["cache_size_bytes"])} / '
                f'{_gib(result["max_cache_size_bytes"])} '
                f'({result["cache_utilization"]:.2f}%)'
            )
        )
        lines.extend(
            [
                "| Metric | Value |",
                "| --- | ---: |",
                f'| Rust hits | {result["rust_hits"]} |',
                f'| Rust misses | {result["rust_misses"]} |',
                f'| Rust hit rate | {result["rust_hit_rate"]:.2f}% |',
                f"| Cache size | {cache_size} |",
            ]
        )
    else:
        lines.append("sccache statistics unavailable.")
    for warning in result["warnings"]:
        lines.extend(("", f"> WARNING: {warning}"))
    with path.open("a", encoding="utf-8", newline="\n") as stream:
        stream.write("\n".join(lines) + "\n\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stats", type=Path, required=True)
    parser.add_argument("--minimum-rust-hit-rate", type=float)
    parser.add_argument("--profile", choices=("test", "release"), default="unspecified")
    parser.add_argument("--github-step-summary", type=Path)
    parser.add_argument("--require-requests", action="store_true")
    args = parser.parse_args()
    try:
        if args.minimum_rust_hit_rate is not None and not 0 <= args.minimum_rust_hit_rate <= 100:
            raise StatsError("minimum Rust hit rate must be between 0 and 100")
        result = summarize(json.loads(args.stats.read_bytes()), args.minimum_rust_hit_rate)
    except (OSError, json.JSONDecodeError, StatsError) as error:
        result = {
            "schema": 1,
            "result": "unavailable",
            "warnings": [str(error)],
        }
    result["profile"] = args.profile
    if args.github_step_summary is not None:
        try:
            append_github_summary(args.github_step_summary, args.profile, result)
        except OSError as error:
            result["warnings"].append(f"cannot write GitHub Step Summary: {error}")
    print(json.dumps(result, indent=2, sort_keys=True))
    if args.require_requests and (
        result["result"] != "reported"
        or result["compile_requests"] == 0
        or result["rust_requests"] == 0
    ):
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
