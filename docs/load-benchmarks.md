# Local load benchmark

`scripts/benchmark-load.sh` provides a repeatable Linux baseline for the local scheduler and HTTP capability. It builds the optimized binary, starts the repository's isolated HTTP fixture, and schedules 5,000 iterations at 1,000 starts per second with an active limit of 256.

The report directory contains:

- `environment.txt`: timestamp, revision and dirty state, OS, CPU, logical CPU count, memory, Rust version, fixture address, and exact command;
- `result.json`: scoped Mettle workload metrics;
- `resources.txt`: elapsed time, process CPU ticks, clock frequency, and sampled peak resident memory;
- `fixture.log`: fixture startup and failure output.

Run it with:

```bash
./scripts/benchmark-load.sh
```

Pass an output directory to retain a named comparison:

```bash
./scripts/benchmark-load.sh target/benchmarks/before-pool-change
```

The fixture and client share one machine, so this measures the complete local setup rather than a protocol-independent maximum. Use the same hardware and fixture for comparisons. Profile CPU or allocations around the command recorded in `environment.txt` when a benchmark identifies a regression; external profiling stays out of the runtime hot path and dependency graph.

The final Milestone 5 run on an AMD Ryzen 7 7800X3D scheduled and completed all 5,000 iterations in 5.04 seconds with no failures or drops. The Mettle process used 0.34 CPU seconds and a sampled peak resident set of 7,604 KiB. Iteration latency was 40.14 ms on average and 42.47 ms at p95. These figures describe that local Python fixture and are a regression baseline, not a general throughput claim.
