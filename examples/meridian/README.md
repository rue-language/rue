# Meridian

Meridian is a relational database, query optimizer, transaction simulator,
recovery engine, and observability toolkit written entirely in Rue. It is a
maintained application example and a deliberately demanding compiler workload.

More than 35,000 lines across 265 Rue modules form a single reachable program
rather than a disconnected source corpus. The command-line entry point pulls every
subsystem into the compiler graph, and the self-test and stress modes execute
each analysis, optimizer, and reporting module twice to check determinism. The
main subsystems are:

- a lexer and predictive parser for a compact SQL-like schema and query language;
- catalog, row, cell, index, pooled-string, and snapshot data structures;
- semantic validation for schemas, references, rows, queries, and transactions;
- scan, filter, join, group, order, limit, cost, and cardinality planning;
- a deterministic query engine checked against an independent row oracle;
- MVCC snapshots, locks, write-ahead logging, checkpoints, undo, crash recovery,
  replication, failover, sharding, partitioning, constraints, and lineage;
- 112 catalog, storage, query, transaction, WAL, replication, and workload
  analysis passes;
- 72 logical, join, physical, and distributed optimizer rules;
- 44 deterministic renderers covering human, machine-readable, tracing,
  monitoring, database, testing, and benchmark formats.

Run the built-in database and query:

```console
scripts/rue exec examples/meridian/main.rue demo
```

Parse, validate, plan, and execute a checked-in SQL workload:

```console
scripts/rue exec examples/meridian/main.rue run examples/meridian/demo.sql
```

Exercise the invariant and scaling workloads:

```console
scripts/rue exec examples/meridian/main.rue selftest
scripts/rue exec examples/meridian/main.rue stress1
scripts/rue exec examples/meridian/main.rue stress2
scripts/rue exec examples/meridian/main.rue stress4
scripts/rue exec examples/meridian/main.rue benchmark
```

Running without arguments prints the command reference. This keeps execution
lightweight after the whole program has been compiled by the repository's
automatic example smoke test.

## SQL-like workload language

```text
CREATE TABLE users 3;
COLUMN users id INT REQUIRED UNIQUE;
COLUMN users team INT REQUIRED;
COLUMN users score INT REQUIRED;
INSERT users 1 7 82;
INSERT users 2 7 91;
CREATE INDEX users_score users score;
BEGIN 7;
ROLLBACK 7;
SELECT users FILTER score 80 GROUP team ORDER score LIMIT 10;
```

Statements end in semicolons. Schemas declare their column count explicitly so
the validator can diagnose missing or extra `COLUMN` statements. The compact
surface accepts integer row values and supports filters, joins, groups, ordering,
limits, indexes, and transaction boundaries.
