#!/usr/bin/env python3
"""Validate machine-readable sccache statistics for one Rust build."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any


class StatsError(RuntimeError):
    pass


def _integer(value: object, label: str) -> int:
    if type(value) is not int or value < 0:
        raise StatsError(f"{label} must be a non-negative integer")
    return value


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


def validate(stats_document: object, minimum_rust_hit_rate: float | None = None) -> dict[str, Any]:
    if not isinstance(stats_document, dict) or not isinstance(stats_document.get("stats"), dict):
        raise StatsError("sccache JSON must contain a stats object")
    stats = stats_document["stats"]
    compile_requests = _integer(stats.get("compile_requests"), "compile_requests")
    hits = _language_count(stats.get("cache_hits"), "Rust")
    misses = _language_count(stats.get("cache_misses"), "Rust")
    errors = _language_count(stats.get("cache_errors"), "Rust")
    read_errors = _integer(stats.get("cache_read_errors"), "cache_read_errors")
    write_errors = _integer(stats.get("cache_write_errors"), "cache_write_errors")
    rust_requests = hits + misses + errors
    if compile_requests == 0 or rust_requests == 0:
        raise StatsError("Rust build recorded zero sccache compile requests")
    if read_errors or write_errors or errors:
        raise StatsError("sccache recorded Rust or cache read/write errors")
    hit_rate = hits * 100.0 / (hits + misses) if hits + misses else 0.0
    if minimum_rust_hit_rate is not None and hit_rate < minimum_rust_hit_rate:
        raise StatsError(
            f"Rust cache hit rate {hit_rate:.2f}% is below {minimum_rust_hit_rate:.2f}%"
        )
    return {
        "schema": 1,
        "result": "pass",
        "compile_requests": compile_requests,
        "rust_requests": rust_requests,
        "rust_hits": hits,
        "rust_misses": misses,
        "rust_hit_rate": round(hit_rate, 2),
        "cache_read_errors": read_errors,
        "cache_write_errors": write_errors,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stats", type=Path, required=True)
    parser.add_argument("--minimum-rust-hit-rate", type=float)
    args = parser.parse_args()
    try:
        if args.minimum_rust_hit_rate is not None and not 0 <= args.minimum_rust_hit_rate <= 100:
            raise StatsError("minimum Rust hit rate must be between 0 and 100")
        result = validate(json.loads(args.stats.read_bytes()), args.minimum_rust_hit_rate)
    except (OSError, json.JSONDecodeError, StatsError) as error:
        print(json.dumps({"schema": 1, "error": str(error)}, indent=2), file=sys.stderr)
        return 2
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
