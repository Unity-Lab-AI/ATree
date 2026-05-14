#!/usr/bin/env bash
# ATree semantic indexing benchmark
# Runs cold and warm indexes on configured repos and outputs a comparison table.
#
# Usage: ./scripts/benchmark.sh [--output bench.json]
#
# Prerequisites: cargo build --release

set -euo pipefail

OUTPUT="${1:---output bench.json}"
ATREE_BIN="./target/release/atree"
BENCH_DB="/tmp/atree_bench_index.sqlite"

# Repos to benchmark (small → large)
declare -a REPOS=(
    "tests/fixtures:ATree fixtures"
)

# Check if we have larger repos available
if [ -d "/home/bamn/GitNexusRelay" ]; then
    REPOS+=("/home/bamn/GitNexusRelay:GitNexusRelay")
fi
if [ -d "/home/bamn/qwen-code" ]; then
    REPOS+=("/home/bamn/qwen-code:qwen-code")
fi

echo "=== ATree Semantic Indexing Benchmark ==="
echo "Binary: $ATREE_BIN"
echo ""

# Build release binary if needed
if [ ! -f "$ATREE_BIN" ]; then
    echo "Building release binary..."
    cargo build --release 2>&1
fi

RESULTS="[]"

for entry in "${REPOS[@]}"; do
    repo_path="${entry%%:*}"
    repo_name="${entry##*:}"

    echo "--- Benchmarking: $repo_name ($repo_path) ---"

    # Cold run (fresh index)
    rm -f "$BENCH_DB"
    echo "  Cold run..."
    cold_start=$(date +%s%N)
    "$ATREE_BIN" --semantic --db "$BENCH_DB" --root "$repo_path" --include-files --json > /dev/null 2>&1
    cold_end=$(date +%s%N)
    cold_ms=$(( (cold_end - cold_start) / 1000000 ))

    # Extract stats from DB
    files=$(sqlite3 "$BENCH_DB" "SELECT COUNT(*) FROM files;" 2>/dev/null || echo 0)
    symbols=$(sqlite3 "$BENCH_DB" "SELECT COUNT(*) FROM symbols;" 2>/dev/null || echo 0)
    calls=$(sqlite3 "$BENCH_DB" "SELECT COUNT(*) FROM calls;" 2>/dev/null || echo 0)
    edges=$(sqlite3 "$BENCH_DB" "SELECT COUNT(*) FROM edges;" 2>/dev/null || echo 0)
    resolved=$(sqlite3 "$BENCH_DB" "SELECT COUNT(*) FROM calls WHERE resolved_symbol_id IS NOT NULL;" 2>/dev/null || echo 0)

    if [ "$calls" -gt 0 ]; then
        resolved_pct=$(echo "scale=1; $resolved * 100 / $calls" | bc 2>/dev/null || echo "N/A")
    else
        resolved_pct="N/A"
    fi

    echo "  Cold: ${cold_ms}ms | Files: $files | Symbols: $symbols | Calls: $calls | Edges: $edges | Resolved: ${resolved_pct}%"

    # Warm run (incremental, no changes)
    echo "  Warm run (incremental)..."
    warm_start=$(date +%s%N)
    "$ATREE_BIN" --semantic --db "$BENCH_DB" --root "$repo_path" --include-files --incremental --json > /dev/null 2>&1
    warm_end=$(date +%s%N)
    warm_ms=$(( (warm_end - warm_start) / 1000000 ))

    echo "  Warm: ${warm_ms}ms"

    speedup=$(echo "scale=1; $cold_ms / $warm_ms" | bc 2>/dev/null || echo "N/A")
    echo "  Speedup: ${speedup}x"

    # Append to results
    RESULTS=$(echo "$RESULTS" | jq -c --arg name "$repo_name" \
        --arg path "$repo_path" \
        --argjson cold_ms "$cold_ms" \
        --argjson warm_ms "$warm_ms" \
        --argjson files "$files" \
        --argjson symbols "$symbols" \
        --argjson calls "$calls" \
        --argjson edges "$edges" \
        --argjson resolved_pct "$resolved_pct" \
        --argjson speedup "$speedup" \
        '. + [{name: $name, path: $path, cold_ms: $cold_ms, warm_ms: $warm_ms, files: $files, symbols: $symbols, calls: $calls, edges: $edges, resolved_pct: $resolved_pct, speedup: $speedup}]')
done

# Output results
echo ""
echo "=== Benchmark Results ==="
echo "$RESULTS" | jq .

# Write to file
if [ "$OUTPUT" = "--output bench.json" ]; then
    OUTPUT="bench.json"
fi
echo "$RESULTS" | jq . > "$OUTPUT"
echo ""
echo "Results written to $OUTPUT"
