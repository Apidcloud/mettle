# Dependency policy and audit

Mettle keeps third-party code concentrated around networking, TLS, byte buffers, and JSON correctness. The parser, compiler, execution plan, runtime interpreter, capability interface, CLI, and VS Code extension are implemented within the repository.

## Direct Rust dependencies

| Package | Use | Licence | Selection notes |
| --- | --- | --- | --- |
| Tokio | Async runtime, sockets, timers, Ctrl+C handling | MIT | Cross-platform runtime; only `net`, runtime, signal, and time features are enabled |
| Hyper | HTTP/1.1 protocol implementation | MIT | Low-level client without a web framework |
| Hyper-util | Tokio adapter and pooled legacy client | MIT | Provides maintained client pooling for Hyper 1.x |
| HTTP-body-util | Request body and response frame helpers | MIT | Used for bounded streaming response reads |
| Hyper-rustls | Hyper/Rustls connector | Apache-2.0 OR ISC OR MIT | Default features disabled; HTTP/1, ring, TLS 1.2, and WebPKI roots selected |
| Rustls | TLS configuration | Apache-2.0 OR ISC OR MIT | Default features disabled; ring, standard library, and TLS 1.2 selected |
| Bytes | HTTP byte buffers | MIT | Shared networking primitive used by Hyper |
| Serde JSON | JSON parsing and serialization | MIT OR Apache-2.0 | Used at the HTTP/JSON boundary and for machine-readable CLI flow discovery |

Default features are disabled for the networking and TLS crates where their feature sets are broad. HTTP/2, native certificate discovery, logging adapters, AWS-LC, proxy discovery, compression, and web-framework features are not enabled.

TLS currently uses Rustls with the ring provider and Mozilla WebPKI roots. The runtime does not require a system OpenSSL installation.

## Locked transitive graph

[`third-party-licenses.md`](third-party-licenses.md) records every registry package resolved by `Cargo.lock` and its declared SPDX licence expression. The allowlist contains permissive licences used by the selected graph, including MIT, Apache-2.0, ISC, BSD-3-Clause, Unicode-3.0, Unlicense, and CDLA-Permissive-2.0.

Validate the lockfile and checked-in report with:

```bash
./scripts/check-licenses.py
```

Update the report deliberately after a dependency change with:

```bash
./scripts/check-licenses.py --write
```

Unknown or newly introduced licence expressions fail the check until reviewed and explicitly added to the allowlist.

The repository itself remains `UNLICENSED` until its public distribution licence is chosen. The extension and Rust package metadata state this directly.
