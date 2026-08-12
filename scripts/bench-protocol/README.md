# `scripts/bench-protocol/` — comparative benchmark harness

Preparation tooling for `docs/benchmark-protocol.md` (protocol version
`1.0.0`). See `docs/benchmark-apparatus.md` for what this comparison unit can
and cannot measure, and why.

Nothing in this directory takes a measurement. Every command here is offline,
cheap, and side-effect free unless it is explicitly asked to write a fixture.

## Commands

```text
python3 scripts/bench-protocol/bench-protocol.py --self-test
python3 scripts/bench-protocol/bench-protocol.py --availability
```

`--self-test` runs every module's self-tests (well under a second, suitable for
CI). `--availability` reports, for the live host, which workloads are runnable,
which are blocked by missing apparatus, and which metrics are unsupported.

Per-module entry points, each with its own `--self-test`:

| Module | Purpose |
| --- | --- |
| `prng.py` | splitmix64 + xoshiro256\*\* reproducible stream for preregistered seeds |
| `fixtures.py` | deterministic W3/W4/W5 payload generators, digests, width self-tests |
| `workloads.py` | workload catalogue with per-workload apparatus requirements |
| `ordering.py` | seeded balanced Latin-square execution order |
| `summaries.py` | nearest-rank percentiles, seeded bootstrap CIs, paired comparisons, Theil-Sen |
| `collectors.py` | Linux cgroup v2 collectors; unsupported reporting for wakeups and GPU memory |
| `driver.py` | child-side benchmark driver and out-of-band oracle records |
| `result_schema.py` | canonical result document schema and validator |
| `prereg.py` | preregistration record generator and readiness check |

Useful one-offs:

```text
python3 scripts/bench-protocol/fixtures.py --digest w3
python3 scripts/bench-protocol/collectors.py --probe
python3 scripts/bench-protocol/ordering.py --seed <seed> --implementations odytty,ghostty --blocks 30
python3 scripts/bench-protocol/prereg.py --generate --run-set-id <id> \
    --order-seed <seed> --bootstrap-seed <other-seed> --implementations odytty,ghostty
python3 scripts/bench-protocol/prereg.py --check <record.json>
python3 scripts/bench-protocol/result_schema.py --validate <result.json> \
    --preregistration <record.json>
```

## Canonical fixture digests

Generator revision: the committed `fixtures.py`. Recompute with
`fixtures.py --digest <name>`; a run set records these in its preregistration.

| Fixture | Bytes | SHA-256 |
| --- | --- | --- |
| `w3` | 64,000,000 | `6115e084c778270394b1111e75ae8d882b1e6e1a61ad7d832b96a9dc42dbf3d2` |
| `w4` | 64,000,000 | `6f536f28c5ec3f965c3600fd1e93701a4557605e1567d0c20210bc27db05bfea` |
| `w5` | 10,895,402 | `e9b4deb6703136053f9b0c4d1387640e248e8f442a3a9e879b0464a6c27b07c4` |

The self-test pins a 1000-record prefix digest of each fixture so a change to a
record rule fails immediately. A changed fixture makes previously published run
sets incomparable, so updating a pinned digest is only correct alongside a
protocol version bump and a fresh run set.

## Design rules

These are the rules the modules enforce on each other. They are recorded here
because each one exists to prevent a specific, tempting mistake.

1. **Seeds are reproducible outside this runtime.** The generator is
   splitmix64 seeding xoshiro256\*\*, specified in exact 64-bit integer
   operations, so a preregistered seed can be replayed by an independent
   reimplementation. `random` is not used.

2. **Apparatus requirements live in data, not prose.** `workloads.py` records
   what each workload physically needs. Five of seven require optical capture,
   so they are declared `skip` / `unavailable-hardware` in preregistration
   before any sample is taken. A self-test fails if W3 or W4 ever lose that
   requirement — throughput endpoints are optical under protocol `1.0.0`, and
   quietly relaxing them to software timing would be the single most damaging
   change possible to this harness's honesty.

3. **Oracle records never travel on the measured stream.** They go to a
   separate descriptor or file. On the pty they would become part of the
   workload being timed, and a terminal that mangled them would corrupt the
   evidence meant to catch it. The sink refuses stdin, stdout, and stderr
   rather than silently falling back.

4. **Non-pass samples carry no number.** The validator refuses any `value` key
   on a `fail`, `invalid`, `skip`, or `unsupported` sample — a structural
   rejection, not a check for zero.

5. **Unsupported means unsupported.** A collector that cannot produce a metric
   with the protocol's semantics reports `unsupported` with a specific reason.
   It never substitutes a nearby number: not context switches for wake events,
   not a vendor counter for attributable GPU memory, not a system-wide figure
   for a process-tree figure.

6. **Preregistration is refused when incomplete.** Unpinned placeholders, a
   dirty checkout, identical ordering and bootstrap seeds, no planned workload,
   or an unplanned workload with no declared skip all make a record unready.

7. **Public safety is tested, not assumed.** The preregistration self-test
   asserts that no machine-identifying value reaches the record. It has already
   caught one real leak: a kernel localversion suffix that reproduced the build
   host's name, which is why only numeric kernel version components are
   published.

8. **Statistics implement the protocol's list and stop there.** No significance
   test, no composite score, no weighted total, no overall winner, no outlier
   rejection, no precision-based early stopping.
